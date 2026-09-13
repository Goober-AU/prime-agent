//! Port of packages/tui/src/autocomplete.ts.

use crate::fuzzy::fuzzy_filter;
use crate::slash_command_context::get_slash_command_context;
use async_trait::async_trait;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const PATH_DELIMITERS: &[char] = &[' ', '\t', '"', '\'', '='];

fn to_display_path(value: &str) -> String {
    value.replace('\\', "/")
}

/// Port of `escapeRegex` - escapes exactly the JavaScript metacharacter set.
fn escape_regex(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if matches!(
            ch,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn build_fd_path_query(query: &str) -> String {
    let normalized = to_display_path(query);
    if !normalized.contains('/') {
        return normalized;
    }

    let has_trailing_separator = normalized.ends_with('/');
    let trimmed = normalized.trim_matches('/');
    if trimmed.is_empty() {
        return normalized;
    }

    let separator_pattern = "[\\\\/]";
    let segments: Vec<String> = trimmed
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(escape_regex)
        .collect();
    if segments.is_empty() {
        return normalized;
    }

    let mut pattern = segments.join(separator_pattern);
    if has_trailing_separator {
        pattern.push_str(separator_pattern);
    }
    pattern
}

fn find_last_delimiter(text: &str) -> Option<usize> {
    let chars: Vec<char> = text.chars().collect();
    for i in (0..chars.len()).rev() {
        if PATH_DELIMITERS.contains(&chars[i]) {
            return Some(i);
        }
    }
    None
}

/// Index of the opening quote of an unclosed quote run, or None.
fn find_unclosed_quote_start(text: &str) -> Option<usize> {
    let chars: Vec<char> = text.chars().collect();
    let mut in_quotes = false;
    let mut quote_start: i64 = -1;
    for (i, ch) in chars.iter().enumerate() {
        if *ch == '"' {
            in_quotes = !in_quotes;
            if in_quotes {
                quote_start = i as i64;
            }
        }
    }
    if in_quotes && quote_start >= 0 {
        Some(quote_start as usize)
    } else {
        None
    }
}

fn is_token_start(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    let chars: Vec<char> = text.chars().collect();
    index > 0 && index <= chars.len() && PATH_DELIMITERS.contains(&chars[index - 1])
}

fn extract_quoted_prefix(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let quote_start = find_unclosed_quote_start(text)?;

    if quote_start > 0 && chars.get(quote_start - 1) == Some(&'@') {
        if !is_token_start(text, quote_start - 1) {
            return None;
        }
        return Some(chars[quote_start - 1..].iter().collect());
    }

    if !is_token_start(text, quote_start) {
        return None;
    }

    Some(chars[quote_start..].iter().collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPathPrefix {
    pub raw_prefix: String,
    pub is_at_prefix: bool,
    pub is_quoted_prefix: bool,
}

fn parse_path_prefix(prefix: &str) -> ParsedPathPrefix {
    if let Some(rest) = prefix.strip_prefix("@\"") {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('"') {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: false,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('@') {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: false,
        };
    }
    ParsedPathPrefix {
        raw_prefix: prefix.to_string(),
        is_at_prefix: false,
        is_quoted_prefix: false,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CompletionValueOptions {
    pub is_directory: bool,
    pub is_at_prefix: bool,
    pub is_quoted_prefix: bool,
}

fn build_completion_value(path: &str, options: CompletionValueOptions) -> String {
    let needs_quotes = options.is_quoted_prefix || path.contains(' ');
    let prefix = if options.is_at_prefix { "@" } else { "" };

    if !needs_quotes {
        return format!("{prefix}{path}");
    }

    format!("{prefix}\"{path}\"")
}
// ---------------------------------------------------------------------------
// `walkDirectoryWithFd`
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkEntry {
    pub path: String,
    pub is_directory: bool,
}

fn fd_available(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    Path::new(path).is_file()
}

/// Poll interval for the abort watcher, mirroring how promptly the TypeScript
/// abort listener can fire.
const FD_ABORT_POLL_MS: u64 = 5;

/// Wait for `child` to exit, killing it as soon as `signal` aborts.
///
/// TS registers `signal.addEventListener("abort", onAbort)` and `onAbort` calls
/// `child.kill("SIGKILL")` while the caller's promise resolves with `[]`
/// (autocomplete.ts:178-184). `wait_with_output` alone cannot do that - it
/// returns only after the child exits - so the wait is polled here and the child
/// is killed on abort. Returns `true` when the wait ended because of the abort.
fn wait_for_child_or_abort(child: &mut std::process::Child, signal: &AbortSignal) -> bool {
    loop {
        if signal.is_aborted() {
            let _ = child.kill();
            let _ = child.wait();
            return true;
        }
        match child.try_wait() {
            Ok(Some(_)) => return false,
            Ok(None) => {}
            Err(_) => return false,
        }
        std::thread::sleep(std::time::Duration::from_millis(FD_ABORT_POLL_MS));
    }
}

/// Port of `walkDirectoryWithFd`. The child's output is drained on helper threads
/// so the wait below can kill the process when `signal` aborts.
pub fn walk_directory_with_fd(
    base_dir: &str,
    fd_path: &str,
    query: &str,
    max_results: usize,
    signal: &AbortSignal,
) -> Vec<WalkEntry> {
    let mut args: Vec<String> = vec![
        "--base-directory".to_string(),
        base_dir.to_string(),
        "--max-results".to_string(),
        max_results.to_string(),
        "--type".to_string(),
        "f".to_string(),
        "--type".to_string(),
        "d".to_string(),
        "--follow".to_string(),
        "--hidden".to_string(),
        "--exclude".to_string(),
        ".git".to_string(),
        "--exclude".to_string(),
        ".git/*".to_string(),
        "--exclude".to_string(),
        ".git/**".to_string(),
    ];

    if to_display_path(query).contains('/') {
        args.push("--full-path".to_string());
    }

    if !query.is_empty() {
        args.push(build_fd_path_query(query));
    }

    if signal.is_aborted() || !fd_available(fd_path) {
        return Vec::new();
    }

    let mut child = match std::process::Command::new(fd_path)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return Vec::new(),
    };

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || read_all(stdout_pipe));
    let stderr_reader = std::thread::spawn(move || read_all(stderr_pipe));

    let aborted = wait_for_child_or_abort(&mut child, signal);
    let stdout = stdout_reader.join().unwrap_or_default();
    let _stderr = stderr_reader.join().unwrap_or_default();

    if aborted || signal.is_aborted() {
        return Vec::new();
    }
    if !matches!(child.try_wait(), Ok(Some(status)) if status.success()) {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&stdout).to_string();
    if stdout.trim().is_empty() {
        return Vec::new();
    }

    let mut results: Vec<WalkEntry> = Vec::new();
    for line in stdout.trim().split('\n').filter(|line| !line.is_empty()) {
        let display_line = to_display_path(line);
        let has_trailing_separator = display_line.ends_with('/');
        let normalized_path = if has_trailing_separator {
            display_line[..display_line.len() - 1].to_string()
        } else {
            display_line.clone()
        };
        if normalized_path == ".git"
            || normalized_path.starts_with(".git/")
            || normalized_path.contains("/.git/")
        {
            continue;
        }

        results.push(WalkEntry {
            path: display_line,
            is_directory: has_trailing_separator,
        });
    }

    results
}

/// Read a piped child stream to the end (empty when the pipe is absent).
fn read_all(pipe: Option<impl std::io::Read>) -> Vec<u8> {
    let mut buffer = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut buffer);
    }
    buffer
}

/// Port of the TypeScript `AbortSignal` surface used by this module.
#[derive(Debug, Clone, Default)]
pub struct AbortSignal {
    aborted: Arc<AtomicBool>,
}

impl AbortSignal {
    pub fn new() -> Self {
        Self {
            aborted: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }

    pub fn is_aborted(&self) -> bool {
        self.aborted()
    }

    pub fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutocompleteItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub source_tag: Option<String>,
    pub takes_argument: Option<bool>,
}

/// Port of the `SlashCommand` interface.
pub struct SlashCommand {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub source_tag: Option<String>,
    pub takes_argument: Option<bool>,
    pub get_argument_completions: Option<Box<dyn Fn(&str) -> Vec<AutocompleteItem>>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutocompleteSuggestions {
    pub items: Vec<AutocompleteItem>,
    pub prefix: String,
    /// "slash-command" | "file" | undefined
    pub kind: Option<String>,
}

/// Port of the `AutocompleteProvider` interface.
#[async_trait]
pub trait AutocompleteProvider {
    async fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        signal: &AbortSignal,
        force: bool,
    ) -> Option<AutocompleteSuggestions>;

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult;

    fn should_trigger_file_completion(&self, lines: &[String], cursor_line: usize, cursor_col: usize) -> bool {
        let _ = (lines, cursor_line, cursor_col);
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyCompletionResult {
    pub lines: Vec<String>,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

/// Port of the `SlashCommand | AutocompleteItem` union.
pub enum CommandEntry {
    Slash(SlashCommand),
    Item(AutocompleteItem),
}

impl CommandEntry {
    pub fn name(&self) -> &str {
        match self {
            CommandEntry::Slash(command) => &command.name,
            CommandEntry::Item(item) => &item.value,
        }
    }

    fn description(&self) -> Option<String> {
        match self {
            CommandEntry::Slash(command) => command.description.clone(),
            CommandEntry::Item(item) => item.description.clone(),
        }
    }

    fn argument_hint(&self) -> Option<String> {
        match self {
            CommandEntry::Slash(command) => command.argument_hint.clone(),
            CommandEntry::Item(item) => item.argument_hint.clone(),
        }
    }

    fn source_tag(&self) -> Option<String> {
        match self {
            CommandEntry::Slash(command) => command.source_tag.clone(),
            CommandEntry::Item(item) => item.source_tag.clone(),
        }
    }

    fn takes_argument(&self) -> bool {
        match self {
            CommandEntry::Slash(command) => command.takes_argument.unwrap_or(false),
            CommandEntry::Item(item) => item.takes_argument.unwrap_or(false),
        }
    }
}
// ---------------------------------------------------------------------------
// `CombinedAutocompleteProvider`
// ---------------------------------------------------------------------------

pub struct CombinedAutocompleteProvider {
    commands: Vec<CommandEntry>,
    base_path: String,
    fd_path: Option<String>,
}

impl CombinedAutocompleteProvider {
    pub fn new(commands: Vec<CommandEntry>, base_path: &str, fd_path: Option<&str>) -> Self {
        Self {
            commands,
            base_path: base_path.to_string(),
            fd_path: fd_path.map(|path| path.to_string()),
        }
    }

    pub async fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        signal: &AbortSignal,
        force: bool,
    ) -> Option<AutocompleteSuggestions> {
        let current_line = lines.get(cursor_line).cloned().unwrap_or_default();
        let text_before_cursor: String = current_line.chars().take(cursor_col).collect();

        if let Some(at_prefix) = self.extract_at_prefix(&text_before_cursor) {
            let parsed = parse_path_prefix(&at_prefix);
            let suggestions = self
                .get_fuzzy_file_suggestions(&parsed.raw_prefix, parsed.is_quoted_prefix, signal)
                .await;
            if suggestions.is_empty() {
                return None;
            }

            return Some(AutocompleteSuggestions {
                items: suggestions,
                prefix: at_prefix,
                kind: Some("file".to_string()),
            });
        }

        let slash_context = if force {
            None
        } else {
            get_slash_command_context(lines, cursor_line, cursor_col)
        };
        match slash_context {
            Some(crate::slash_command_context::SlashCommandContext::Name { prefix, .. }) => {
                let prefix_without_slash: String = prefix.chars().skip(1).collect();
                let command_items: Vec<(String, String, Option<String>, Option<String>, Option<String>, bool)> =
                    self.commands
                        .iter()
                        .map(|cmd| {
                            let name = cmd.name().to_string();
                            let aliases = match cmd {
                                CommandEntry::Slash(command) => command.aliases.clone(),
                                CommandEntry::Item(_) => Vec::new(),
                            };
                            let search_text = std::iter::once(name.clone())
                                .chain(aliases.into_iter())
                                .collect::<Vec<String>>()
                                .join(" ");
                            (
                                name,
                                search_text,
                                cmd.description(),
                                cmd.argument_hint(),
                                cmd.source_tag(),
                                cmd.takes_argument(),
                            )
                        })
                        .collect();

                let filtered: Vec<AutocompleteItem> = fuzzy_filter(&command_items, &prefix_without_slash, &|item| {
                    item.1.clone()
                })
                .into_iter()
                .map(|item| {
                    let mut suggestion = AutocompleteItem {
                        value: item.0.clone(),
                        label: item.0.clone(),
                        description: None,
                        argument_hint: None,
                        source_tag: None,
                        takes_argument: None,
                    };
                    if let Some(description) = item.2.clone() {
                        suggestion.description = Some(description);
                    }
                    if let Some(argument_hint) = item.3.clone() {
                        suggestion.argument_hint = Some(argument_hint);
                    }
                    if let Some(source_tag) = item.4.clone() {
                        suggestion.source_tag = Some(source_tag);
                    }
                    if item.5 {
                        suggestion.takes_argument = Some(true);
                    }
                    suggestion
                })
                .collect();

                if filtered.is_empty() {
                    return None;
                }

                Some(AutocompleteSuggestions {
                    items: filtered,
                    prefix,
                    kind: Some("slash-command".to_string()),
                })
            }
            Some(crate::slash_command_context::SlashCommandContext::Argument {
                command_name,
                prefix,
                ..
            }) => {
                let command = self.commands.iter().find(|cmd| cmd.name() == command_name)?;
                let completions = match command {
                    CommandEntry::Slash(slash) => slash.get_argument_completions.as_ref(),
                    CommandEntry::Item(_) => None,
                };
                let completions = completions?;
                let argument_suggestions = completions(&prefix);
                if argument_suggestions.is_empty() {
                    return None;
                }

                Some(AutocompleteSuggestions {
                    items: argument_suggestions,
                    prefix,
                    kind: None,
                })
            }
            None => {
                let path_match = self.extract_path_prefix(&text_before_cursor, force)?;
                let suggestions = self.get_file_suggestions(&path_match);
                if suggestions.is_empty() {
                    return None;
                }

                Some(AutocompleteSuggestions {
                    items: suggestions,
                    prefix: path_match,
                    kind: Some("file".to_string()),
                })
            }
        }
    }

    pub fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        let current_line = lines.get(cursor_line).cloned().unwrap_or_default();
        let before_prefix: String = current_line.chars().take(cursor_col.saturating_sub(prefix.chars().count())).collect();
        let after_cursor: String = current_line.chars().skip(cursor_col).collect();
        let is_quoted_prefix = prefix.starts_with('"') || prefix.starts_with("@\"");
        let has_leading_quote_after_cursor = after_cursor.starts_with('"');
        let has_trailing_quote_in_item = item.value.ends_with('"');
        let adjusted_after_cursor = if is_quoted_prefix && has_trailing_quote_in_item && has_leading_quote_after_cursor
        {
            after_cursor.chars().skip(1).collect::<String>()
        } else {
            after_cursor.clone()
        };
        let slash_context = get_slash_command_context(lines, cursor_line, cursor_col);

        let is_slash_command = matches!(
            &slash_context,
            Some(crate::slash_command_context::SlashCommandContext::Name { prefix: context_prefix, .. })
                if context_prefix == prefix
        ) && self.commands.iter().any(|command| command.name() == item.value);
        if is_slash_command {
            let has_separator_after_cursor = adjusted_after_cursor.starts_with(' ')
                || adjusted_after_cursor.starts_with('\t');
            let separator = if has_separator_after_cursor { "" } else { " " };
            let new_line = format!("{before_prefix}/{}{separator}{adjusted_after_cursor}", item.value);
            let mut new_lines = lines.to_vec();
            if cursor_line < new_lines.len() {
                new_lines[cursor_line] = new_line;
            }

            return ApplyCompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix.chars().count() + item.value.chars().count() + 2,
            };
        }

        if prefix.starts_with('@') {
            // Don't add space after directories so user can continue autocompleting
            let is_directory = item.label.ends_with('/');
            let suffix = if is_directory { "" } else { " " };
            let new_line = format!("{before_prefix}{}{suffix}{adjusted_after_cursor}", item.value);
            let mut new_lines = lines.to_vec();
            if cursor_line < new_lines.len() {
                new_lines[cursor_line] = new_line;
            }

            let has_trailing_quote = item.value.ends_with('"');
            let cursor_offset = if is_directory && has_trailing_quote {
                item.value.chars().count() - 1
            } else {
                item.value.chars().count()
            };

            return ApplyCompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix.chars().count() + cursor_offset + suffix.chars().count(),
            };
        }

        if let Some(crate::slash_command_context::SlashCommandContext::Argument { prefix: context_prefix, .. }) =
            &slash_context
        {
            if context_prefix == prefix {
                let new_line = format!("{before_prefix}{}{adjusted_after_cursor}", item.value);
                let mut new_lines = lines.to_vec();
                if cursor_line < new_lines.len() {
                    new_lines[cursor_line] = new_line;
                }

                let is_directory = item.label.ends_with('/');
                let has_trailing_quote = item.value.ends_with('"');
                let cursor_offset = if is_directory && has_trailing_quote {
                    item.value.chars().count() - 1
                } else {
                    item.value.chars().count()
                };

                return ApplyCompletionResult {
                    lines: new_lines,
                    cursor_line,
                    cursor_col: before_prefix.chars().count() + cursor_offset,
                };
            }
        }

        let new_line = format!("{before_prefix}{}{adjusted_after_cursor}", item.value);
        let mut new_lines = lines.to_vec();
        if cursor_line < new_lines.len() {
            new_lines[cursor_line] = new_line;
        }

        let is_directory = item.label.ends_with('/');
        let has_trailing_quote = item.value.ends_with('"');
        let cursor_offset = if is_directory && has_trailing_quote {
            item.value.chars().count() - 1
        } else {
            item.value.chars().count()
        };

        ApplyCompletionResult {
            lines: new_lines,
            cursor_line,
            cursor_col: before_prefix.chars().count() + cursor_offset,
        }
    }
    fn extract_at_prefix(&self, text: &str) -> Option<String> {
        let quoted_prefix = extract_quoted_prefix(text);
        if let Some(quoted) = &quoted_prefix {
            if quoted.starts_with("@\"") {
                return quoted_prefix;
            }
        }

        let chars: Vec<char> = text.chars().collect();
        let last_delimiter_index = find_last_delimiter(text);
        let token_start = match last_delimiter_index {
            None => 0,
            Some(index) => index + 1,
        };

        if chars.get(token_start) == Some(&'@') {
            return Some(chars[token_start..].iter().collect());
        }

        None
    }

    fn extract_path_prefix(&self, text: &str, force_extract: bool) -> Option<String> {
        if let Some(quoted_prefix) = extract_quoted_prefix(text) {
            return Some(quoted_prefix);
        }

        let chars: Vec<char> = text.chars().collect();
        let last_delimiter_index = find_last_delimiter(text);
        let path_prefix: String = match last_delimiter_index {
            None => text.to_string(),
            Some(index) => chars[index + 1..].iter().collect(),
        };

        if force_extract {
            return Some(path_prefix);
        }

        // For natural triggers, return if it looks like a path, ends with /, starts with ~/, .
        if path_prefix.contains('/') || path_prefix.starts_with('.') || path_prefix.starts_with("~/") {
            return Some(path_prefix);
        }

        if path_prefix.is_empty() && text.ends_with(' ') {
            return Some(path_prefix);
        }

        None
    }

    fn expand_home_path(&self, path: &str) -> String {
        if let Some(rest) = path.strip_prefix("~/") {
            let home = home_dir();
            let expanded_path = join_path(&home, rest);
            if path.ends_with('/') && !expanded_path.ends_with('/') {
                return format!("{expanded_path}/");
            }
            return expanded_path;
        }
        if path == "~" {
            return home_dir();
        }
        path.to_string()
    }

    fn resolve_scoped_fuzzy_query(&self, raw_query: &str) -> Option<ScopedQuery> {
        let normalized_query = to_display_path(raw_query);
        let slash_index = normalized_query.rfind('/')?;

        let display_base = normalized_query[..slash_index + 1].to_string();
        let query = normalized_query[slash_index + 1..].to_string();

        let base_dir = if display_base.starts_with("~/") {
            self.expand_home_path(&display_base)
        } else if display_base.starts_with('/') {
            display_base.clone()
        } else {
            join_path(&self.base_path, &display_base)
        };

        let metadata = std::fs::metadata(&base_dir).ok()?;
        if !metadata.is_dir() {
            return None;
        }

        Some(ScopedQuery {
            base_dir,
            query,
            display_base,
        })
    }

    fn scoped_path_for_display(&self, display_base: &str, relative_path: &str) -> String {
        let normalized_relative_path = to_display_path(relative_path);
        if display_base == "/" {
            return format!("/{normalized_relative_path}");
        }
        format!("{}{normalized_relative_path}", to_display_path(display_base))
    }

    fn get_file_suggestions(&self, prefix: &str) -> Vec<AutocompleteItem> {
        let parsed = parse_path_prefix(prefix);
        let raw_prefix = parsed.raw_prefix.clone();
        let is_at_prefix = parsed.is_at_prefix;
        let is_quoted_prefix = parsed.is_quoted_prefix;
        let mut expanded_prefix = raw_prefix.clone();

        if expanded_prefix.starts_with('~') {
            expanded_prefix = self.expand_home_path(&expanded_prefix);
        }

        let is_root_prefix = raw_prefix.is_empty()
            || raw_prefix == "./"
            || raw_prefix == "../"
            || raw_prefix == "~"
            || raw_prefix == "~/"
            || raw_prefix == "/"
            || (is_at_prefix && raw_prefix.is_empty());

        let search_dir: String;
        let search_prefix: String;
        if is_root_prefix || raw_prefix.ends_with('/') {
            search_dir = if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                expanded_prefix.clone()
            } else {
                join_path(&self.base_path, &expanded_prefix)
            };
            search_prefix = String::new();
        } else {
            let dir = dirname(&expanded_prefix);
            let file = basename(&expanded_prefix);
            search_dir = if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                dir
            } else {
                join_path(&self.base_path, &dir)
            };
            search_prefix = file;
        }

        let entries = match std::fs::read_dir(&search_dir) {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };

        let mut suggestions: Vec<AutocompleteItem> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.to_lowercase().starts_with(&search_prefix.to_lowercase()) {
                continue;
            }

            let file_type = entry.file_type().ok();
            let mut is_directory = file_type.map(|t| t.is_dir()).unwrap_or(false);
            if !is_directory && file_type.map(|t| t.is_symlink()).unwrap_or(false) {
                if let Ok(metadata) = std::fs::metadata(entry.path()) {
                    is_directory = metadata.is_dir();
                }
            }

            let display_prefix = raw_prefix.clone();
            let relative_path: String = if display_prefix.ends_with('/') {
                format!("{display_prefix}{name}")
            } else if display_prefix.contains('/') || display_prefix.contains('\\') {
                if display_prefix.starts_with("~/") {
                    let home_relative_dir: String = display_prefix.chars().skip(2).collect();
                    let dir = dirname(&home_relative_dir);
                    let joined = if dir == "." {
                        name.clone()
                    } else {
                        join_path(&dir, &name)
                    };
                    format!("~/{joined}")
                } else if display_prefix.starts_with('/') {
                    let dir = dirname(&display_prefix);
                    if dir == "/" {
                        format!("/{name}")
                    } else {
                        format!("{dir}/{name}")
                    }
                } else {
                    let mut joined = join_path(&dirname(&display_prefix), &name);
                    if display_prefix.starts_with("./") && !joined.starts_with("./") {
                        joined = format!("./{joined}");
                    }
                    joined
                }
            } else if display_prefix.starts_with('~') {
                format!("~/{name}")
            } else {
                name.clone()
            };

            let relative_path = to_display_path(&relative_path);
            let path_value = if is_directory {
                format!("{relative_path}/")
            } else {
                relative_path
            };
            let value = build_completion_value(
                &path_value,
                CompletionValueOptions {
                    is_directory,
                    is_at_prefix,
                    is_quoted_prefix,
                },
            );

            suggestions.push(AutocompleteItem {
                value,
                label: format!("{name}{}", if is_directory { "/" } else { "" }),
                description: None,
                argument_hint: None,
                source_tag: None,
                takes_argument: None,
            });
        }

        suggestions.sort_by(|a, b| {
            let a_is_dir = a.value.ends_with('/');
            let b_is_dir = b.value.ends_with('/');
            if a_is_dir && !b_is_dir {
                return std::cmp::Ordering::Less;
            }
            if !a_is_dir && b_is_dir {
                return std::cmp::Ordering::Greater;
            }
            locale_compare(&a.label, &b.label)
        });

        suggestions
    }

    fn score_entry(&self, file_path: &str, query: &str, is_directory: bool) -> i64 {
        let file_name = basename(file_path);
        let lower_file_name = file_name.to_lowercase();
        let lower_query = query.to_lowercase();

        let mut score: i64 = 0;

        if lower_file_name == lower_query {
            score = 100;
        } else if lower_file_name.starts_with(&lower_query) {
            score = 80;
        } else if lower_file_name.contains(&lower_query) {
            score = 50;
        } else if file_path.to_lowercase().contains(&lower_query) {
            score = 30;
        }

        if is_directory && score > 0 {
            score += 10;
        }

        score
    }

    async fn get_fuzzy_file_suggestions(
        &self,
        query: &str,
        is_quoted_prefix: bool,
        signal: &AbortSignal,
    ) -> Vec<AutocompleteItem> {
        let fd_path = match &self.fd_path {
            Some(path) if !path.is_empty() => path.clone(),
            _ => return Vec::new(),
        };
        if signal.is_aborted() {
            return Vec::new();
        }

        let scoped_query = self.resolve_scoped_fuzzy_query(query);
        let fd_base_dir = scoped_query
            .as_ref()
            .map(|scoped| scoped.base_dir.clone())
            .unwrap_or_else(|| self.base_path.clone());
        let fd_query = scoped_query
            .as_ref()
            .map(|scoped| scoped.query.clone())
            .unwrap_or_else(|| query.to_string());
        let entries = walk_directory_with_fd(&fd_base_dir, &fd_path, &fd_query, 100, signal);
        if signal.is_aborted() {
            return Vec::new();
        }

        let mut scored_entries: Vec<(WalkEntry, i64)> = entries
            .into_iter()
            .map(|entry| {
                let score = if fd_query.is_empty() {
                    1
                } else {
                    self.score_entry(&entry.path, &fd_query, entry.is_directory)
                };
                (entry, score)
            })
            .filter(|(_, score)| *score > 0)
            .collect();

        scored_entries.sort_by(|a, b| b.1.cmp(&a.1));
        let top_entries: Vec<WalkEntry> = scored_entries
            .into_iter()
            .take(20)
            .map(|(entry, _)| entry)
            .collect();

        let mut suggestions: Vec<AutocompleteItem> = Vec::new();
        for entry in top_entries {
            let path_without_slash = if entry.is_directory {
                entry.path[..entry.path.len() - 1].to_string()
            } else {
                entry.path.clone()
            };
            let display_path = match &scoped_query {
                Some(scoped) => self.scoped_path_for_display(&scoped.display_base, &path_without_slash),
                None => path_without_slash.clone(),
            };
            let entry_name = basename(&path_without_slash);
            let completion_path = if entry.is_directory {
                format!("{display_path}/")
            } else {
                display_path.clone()
            };
            let value = build_completion_value(
                &completion_path,
                CompletionValueOptions {
                    is_directory: entry.is_directory,
                    is_at_prefix: true,
                    is_quoted_prefix,
                },
            );

            suggestions.push(AutocompleteItem {
                value,
                label: format!("{entry_name}{}", if entry.is_directory { "/" } else { "" }),
                description: Some(display_path),
                argument_hint: None,
                source_tag: None,
                takes_argument: None,
            });
        }

        suggestions
    }

    pub fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        !matches!(
            get_slash_command_context(lines, cursor_line, cursor_col),
            Some(crate::slash_command_context::SlashCommandContext::Name { .. })
        )
    }
}

struct ScopedQuery {
    base_dir: String,
    query: String,
    display_base: String,
}

/// Port of `os.homedir()`.
fn home_dir() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Platform path helpers
//
// The TypeScript module imports Node's `basename`, `dirname` and `join`
// (`autocomplete.ts:4`: `import { basename, dirname, join } from "path";`), so
// the port must reproduce the *platform* flavour of those helpers: `path.win32`
// on Windows and `path.posix` elsewhere. Node's win32 flavour treats both `\`
// and `/` as separators and composes with `\`; the previous POSIX-only split
// broke `src\fo` + Tab, `\` + Tab and relative composition (TUIR-9). The
// helpers below transliterate Node 24's `lib/path.js` implementation, including
// its root/device handling and `normalizeString` `.`/`..` resolution.
// ---------------------------------------------------------------------------

/// `isPathSeparator` - true for `/` and `\`.
fn is_path_separator(ch: char) -> bool {
    ch == '/' || ch == '\\'
}

/// `WINDOWS_RESERVED_NAMES` / `isWindowsReservedName`.
const WINDOWS_RESERVED_NAMES: [&str; 28] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
    "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "COM\u{b9}", "COM\u{b2}", "COM\u{b3}",
    "LPT\u{b9}", "LPT\u{b2}", "LPT\u{b3}",
];

/// `isWindowsReservedName(path, colonIndex)`; `None` mirrors a JS `indexOf`
/// result of `-1`, where `slice(0, -1)` drops the last character.
fn is_windows_reserved_name(path: &str, colon_index: Option<usize>) -> bool {
    let chars: Vec<char> = path.chars().collect();
    let end = match colon_index {
        Some(index) => index,
        None => match chars.len().checked_sub(1) {
            Some(end) => end,
            None => return false,
        },
    };
    if end > chars.len() {
        return false;
    }
    let device_part: String = chars[..end].iter().collect::<String>().to_uppercase();
    WINDOWS_RESERVED_NAMES.contains(&device_part.as_str())
}

/// `normalizeString` - resolves `.` and `..` segments.
fn normalize_string(path: &str, allow_above_root: bool, separator: char, posix: bool) -> String {
    let chars: Vec<char> = path.chars().collect();
    let is_separator = |ch: char| if posix { ch == '/' } else { is_path_separator(ch) };
    let mut res: Vec<char> = Vec::new();
    let mut last_segment_length: i64 = 0;
    let mut last_slash: i64 = -1;
    let mut dots: i64 = 0;
    let mut code: char = '\0';
    let length = chars.len() as i64;
    let mut i: i64 = 0;
    while i <= length {
        if i < length {
            code = chars[i as usize];
        } else if is_separator(code) {
            break;
        } else {
            code = '/';
        }

        if is_separator(code) {
            if last_slash == i - 1 || dots == 1 {
                // NOOP
            } else if dots == 2 {
                if res.len() < 2
                    || last_segment_length != 2
                    || res[res.len() - 1] != '.'
                    || res[res.len() - 2] != '.'
                {
                    if res.len() > 2 {
                        let last_slash_index = res.len() as i64 - last_segment_length - 1;
                        if last_slash_index == -1 {
                            res.clear();
                            last_segment_length = 0;
                        } else {
                            res.truncate(last_slash_index as usize);
                            let last_index_of = res
                                .iter()
                                .rposition(|ch| *ch == separator)
                                .map(|index| index as i64)
                                .unwrap_or(-1);
                            last_segment_length = res.len() as i64 - 1 - last_index_of;
                        }
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    } else if !res.is_empty() {
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if !res.is_empty() {
                        res.push(separator);
                    }
                    res.push('.');
                    res.push('.');
                    last_segment_length = 2;
                }
            } else {
                if !res.is_empty() {
                    res.push(separator);
                }
                res.extend(chars[(last_slash + 1) as usize..i as usize].iter());
                last_segment_length = i - last_slash - 1;
            }
            last_slash = i;
            dots = 0;
        } else if code == '.' && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        i += 1;
    }
    res.into_iter().collect()
}

// ---------------------------------------------------------------------------
// ICU collation tables (see `locale_compare`)
//
// Measured from Node 24's default `Intl.Collator` with `sensitivity: "base"`
// - the collator `String#localeCompare` uses (autocomplete.ts:666). Every code
// point in one primary group shares a primary weight; the weight is the group
// index + 1. All tables are sorted by code point for binary search.
// ---------------------------------------------------------------------------

/// Primary weight per code point for the punctuation/digit/Latin block.
const ICU_PRIMARY_CHARS: [(char, u8); 288] = [
    (' ', 1),
    ('!', 7),
    ('\"', 11),
    ('#', 23),
    ('$', 33),
    ('%', 24),
    ('&', 22),
    ('\'', 10),
    ('(', 12),
    (')', 13),
    ('*', 19),
    ('+', 27),
    (',', 4),
    ('-', 3),
    ('.', 9),
    ('/', 20),
    ('0', 34),
    ('1', 35),
    ('2', 36),
    ('3', 37),
    ('4', 38),
    ('5', 39),
    ('6', 40),
    ('7', 41),
    ('8', 42),
    ('9', 43),
    (':', 6),
    (';', 5),
    ('<', 28),
    ('=', 29),
    ('>', 30),
    ('?', 8),
    ('@', 18),
    ('A', 44),
    ('B', 46),
    ('C', 47),
    ('D', 48),
    ('E', 49),
    ('F', 50),
    ('G', 51),
    ('H', 52),
    ('I', 53),
    ('J', 56),
    ('K', 57),
    ('L', 58),
    ('M', 59),
    ('N', 60),
    ('O', 62),
    ('P', 64),
    ('Q', 65),
    ('R', 67),
    ('S', 68),
    ('T', 70),
    ('U', 72),
    ('V', 73),
    ('W', 74),
    ('X', 75),
    ('Y', 76),
    ('Z', 77),
    ('[', 14),
    ('\\', 21),
    (']', 15),
    ('^', 26),
    ('_', 2),
    ('`', 25),
    ('a', 44),
    ('b', 46),
    ('c', 47),
    ('d', 48),
    ('e', 49),
    ('f', 50),
    ('g', 51),
    ('h', 52),
    ('i', 53),
    ('j', 56),
    ('k', 57),
    ('l', 58),
    ('m', 59),
    ('n', 60),
    ('o', 62),
    ('p', 64),
    ('q', 65),
    ('r', 67),
    ('s', 68),
    ('t', 70),
    ('u', 72),
    ('v', 73),
    ('w', 74),
    ('x', 75),
    ('y', 76),
    ('z', 77),
    ('{', 16),
    ('|', 31),
    ('}', 17),
    ('~', 32),
    ('ª', 44),
    ('µ', 80),
    ('º', 62),
    ('À', 44),
    ('Á', 44),
    ('Â', 44),
    ('Ã', 44),
    ('Ä', 44),
    ('Å', 44),
    ('Æ', 45),
    ('Ç', 47),
    ('È', 49),
    ('É', 49),
    ('Ê', 49),
    ('Ë', 49),
    ('Ì', 53),
    ('Í', 53),
    ('Î', 53),
    ('Ï', 53),
    ('Ð', 48),
    ('Ñ', 60),
    ('Ò', 62),
    ('Ó', 62),
    ('Ô', 62),
    ('Õ', 62),
    ('Ö', 62),
    ('Ø', 62),
    ('Ù', 72),
    ('Ú', 72),
    ('Û', 72),
    ('Ü', 72),
    ('Ý', 76),
    ('Þ', 78),
    ('ß', 69),
    ('à', 44),
    ('á', 44),
    ('â', 44),
    ('ã', 44),
    ('ä', 44),
    ('å', 44),
    ('æ', 45),
    ('ç', 47),
    ('è', 49),
    ('é', 49),
    ('ê', 49),
    ('ë', 49),
    ('ì', 53),
    ('í', 53),
    ('î', 53),
    ('ï', 53),
    ('ð', 48),
    ('ñ', 60),
    ('ò', 62),
    ('ó', 62),
    ('ô', 62),
    ('õ', 62),
    ('ö', 62),
    ('ø', 62),
    ('ù', 72),
    ('ú', 72),
    ('û', 72),
    ('ü', 72),
    ('ý', 76),
    ('þ', 78),
    ('ÿ', 76),
    ('Ā', 44),
    ('ā', 44),
    ('Ă', 44),
    ('ă', 44),
    ('Ą', 44),
    ('ą', 44),
    ('Ć', 47),
    ('ć', 47),
    ('Ĉ', 47),
    ('ĉ', 47),
    ('Ċ', 47),
    ('ċ', 47),
    ('Č', 47),
    ('č', 47),
    ('Ď', 48),
    ('ď', 48),
    ('Đ', 48),
    ('đ', 48),
    ('Ē', 49),
    ('ē', 49),
    ('Ĕ', 49),
    ('ĕ', 49),
    ('Ė', 49),
    ('ė', 49),
    ('Ę', 49),
    ('ę', 49),
    ('Ě', 49),
    ('ě', 49),
    ('Ĝ', 51),
    ('ĝ', 51),
    ('Ğ', 51),
    ('ğ', 51),
    ('Ġ', 51),
    ('ġ', 51),
    ('Ģ', 51),
    ('ģ', 51),
    ('Ĥ', 52),
    ('ĥ', 52),
    ('Ħ', 52),
    ('ħ', 52),
    ('Ĩ', 53),
    ('ĩ', 53),
    ('Ī', 53),
    ('ī', 53),
    ('Ĭ', 53),
    ('ĭ', 53),
    ('Į', 53),
    ('į', 53),
    ('İ', 53),
    ('ı', 55),
    ('Ĳ', 54),
    ('ĳ', 54),
    ('Ĵ', 56),
    ('ĵ', 56),
    ('Ķ', 57),
    ('ķ', 57),
    ('ĸ', 66),
    ('Ĺ', 58),
    ('ĺ', 58),
    ('Ļ', 58),
    ('ļ', 58),
    ('Ľ', 58),
    ('ľ', 58),
    ('Ŀ', 58),
    ('ŀ', 58),
    ('Ł', 58),
    ('ł', 58),
    ('Ń', 60),
    ('ń', 60),
    ('Ņ', 60),
    ('ņ', 60),
    ('Ň', 60),
    ('ň', 60),
    ('ŉ', 79),
    ('Ŋ', 61),
    ('ŋ', 61),
    ('Ō', 62),
    ('ō', 62),
    ('Ŏ', 62),
    ('ŏ', 62),
    ('Ő', 62),
    ('ő', 62),
    ('Œ', 63),
    ('œ', 63),
    ('Ŕ', 67),
    ('ŕ', 67),
    ('Ŗ', 67),
    ('ŗ', 67),
    ('Ř', 67),
    ('ř', 67),
    ('Ś', 68),
    ('ś', 68),
    ('Ŝ', 68),
    ('ŝ', 68),
    ('Ş', 68),
    ('ş', 68),
    ('Š', 68),
    ('š', 68),
    ('Ţ', 70),
    ('ţ', 70),
    ('Ť', 70),
    ('ť', 70),
    ('Ŧ', 71),
    ('ŧ', 71),
    ('Ũ', 72),
    ('ũ', 72),
    ('Ū', 72),
    ('ū', 72),
    ('Ŭ', 72),
    ('ŭ', 72),
    ('Ů', 72),
    ('ů', 72),
    ('Ű', 72),
    ('ű', 72),
    ('Ų', 72),
    ('ų', 72),
    ('Ŵ', 74),
    ('ŵ', 74),
    ('Ŷ', 76),
    ('ŷ', 76),
    ('Ÿ', 76),
    ('Ź', 77),
    ('ź', 77),
    ('Ż', 77),
    ('ż', 77),
    ('Ž', 77),
    ('ž', 77),
    ('ſ', 68),
];

/// ICU secondary-level order of combining marks. `String#localeCompare` sorts
/// accented forms by this order (measured: acute < grave < breve < ...).
const ICU_MARK_ORDER: [u32; 17] = [
    0x301, 0x300, 0x306, 0x302, 0x30C, 0x30A, 0x308, 0x30B, 0x303, 0x307, 0x327, 0x328, 0x304, 0x30F, 0x311, 0x31B, 0x326,
];

/// Measured decomposition of accented Latin letters: the primary weight of the
/// base letter and the combining marks it decomposes into.
const ICU_LATIN_FOLDS: [(char, u8, &[u32]); 367] = [
    ('À', 44, &[0x300]),
    ('Á', 44, &[0x301]),
    ('Â', 44, &[0x302]),
    ('Ã', 44, &[0x303]),
    ('Ä', 44, &[0x308]),
    ('Å', 44, &[0x30A]),
    ('Ç', 47, &[0x327]),
    ('È', 49, &[0x300]),
    ('É', 49, &[0x301]),
    ('Ê', 49, &[0x302]),
    ('Ë', 49, &[0x308]),
    ('Ì', 53, &[0x300]),
    ('Í', 53, &[0x301]),
    ('Î', 53, &[0x302]),
    ('Ï', 53, &[0x308]),
    ('Ñ', 60, &[0x303]),
    ('Ò', 62, &[0x300]),
    ('Ó', 62, &[0x301]),
    ('Ô', 62, &[0x302]),
    ('Õ', 62, &[0x303]),
    ('Ö', 62, &[0x308]),
    ('Ù', 72, &[0x300]),
    ('Ú', 72, &[0x301]),
    ('Û', 72, &[0x302]),
    ('Ü', 72, &[0x308]),
    ('Ý', 76, &[0x301]),
    ('à', 44, &[0x300]),
    ('á', 44, &[0x301]),
    ('â', 44, &[0x302]),
    ('ã', 44, &[0x303]),
    ('ä', 44, &[0x308]),
    ('å', 44, &[0x30A]),
    ('ç', 47, &[0x327]),
    ('è', 49, &[0x300]),
    ('é', 49, &[0x301]),
    ('ê', 49, &[0x302]),
    ('ë', 49, &[0x308]),
    ('ì', 53, &[0x300]),
    ('í', 53, &[0x301]),
    ('î', 53, &[0x302]),
    ('ï', 53, &[0x308]),
    ('ñ', 60, &[0x303]),
    ('ò', 62, &[0x300]),
    ('ó', 62, &[0x301]),
    ('ô', 62, &[0x302]),
    ('õ', 62, &[0x303]),
    ('ö', 62, &[0x308]),
    ('ù', 72, &[0x300]),
    ('ú', 72, &[0x301]),
    ('û', 72, &[0x302]),
    ('ü', 72, &[0x308]),
    ('ý', 76, &[0x301]),
    ('ÿ', 76, &[0x308]),
    ('Ā', 44, &[0x304]),
    ('ā', 44, &[0x304]),
    ('Ă', 44, &[0x306]),
    ('ă', 44, &[0x306]),
    ('Ą', 44, &[0x328]),
    ('ą', 44, &[0x328]),
    ('Ć', 47, &[0x301]),
    ('ć', 47, &[0x301]),
    ('Ĉ', 47, &[0x302]),
    ('ĉ', 47, &[0x302]),
    ('Ċ', 47, &[0x307]),
    ('ċ', 47, &[0x307]),
    ('Č', 47, &[0x30C]),
    ('č', 47, &[0x30C]),
    ('Ď', 48, &[0x30C]),
    ('ď', 48, &[0x30C]),
    ('Ē', 49, &[0x304]),
    ('ē', 49, &[0x304]),
    ('Ĕ', 49, &[0x306]),
    ('ĕ', 49, &[0x306]),
    ('Ė', 49, &[0x307]),
    ('ė', 49, &[0x307]),
    ('Ę', 49, &[0x328]),
    ('ę', 49, &[0x328]),
    ('Ě', 49, &[0x30C]),
    ('ě', 49, &[0x30C]),
    ('Ĝ', 51, &[0x302]),
    ('ĝ', 51, &[0x302]),
    ('Ğ', 51, &[0x306]),
    ('ğ', 51, &[0x306]),
    ('Ġ', 51, &[0x307]),
    ('ġ', 51, &[0x307]),
    ('Ģ', 51, &[0x327]),
    ('ģ', 51, &[0x327]),
    ('Ĥ', 52, &[0x302]),
    ('ĥ', 52, &[0x302]),
    ('Ĩ', 53, &[0x303]),
    ('ĩ', 53, &[0x303]),
    ('Ī', 53, &[0x304]),
    ('ī', 53, &[0x304]),
    ('Ĭ', 53, &[0x306]),
    ('ĭ', 53, &[0x306]),
    ('Į', 53, &[0x328]),
    ('į', 53, &[0x328]),
    ('İ', 53, &[0x307]),
    ('Ĵ', 56, &[0x302]),
    ('ĵ', 56, &[0x302]),
    ('Ķ', 57, &[0x327]),
    ('ķ', 57, &[0x327]),
    ('Ĺ', 58, &[0x301]),
    ('ĺ', 58, &[0x301]),
    ('Ļ', 58, &[0x327]),
    ('ļ', 58, &[0x327]),
    ('Ľ', 58, &[0x30C]),
    ('ľ', 58, &[0x30C]),
    ('Ń', 60, &[0x301]),
    ('ń', 60, &[0x301]),
    ('Ņ', 60, &[0x327]),
    ('ņ', 60, &[0x327]),
    ('Ň', 60, &[0x30C]),
    ('ň', 60, &[0x30C]),
    ('Ō', 62, &[0x304]),
    ('ō', 62, &[0x304]),
    ('Ŏ', 62, &[0x306]),
    ('ŏ', 62, &[0x306]),
    ('Ő', 62, &[0x30B]),
    ('ő', 62, &[0x30B]),
    ('Ŕ', 67, &[0x301]),
    ('ŕ', 67, &[0x301]),
    ('Ŗ', 67, &[0x327]),
    ('ŗ', 67, &[0x327]),
    ('Ř', 67, &[0x30C]),
    ('ř', 67, &[0x30C]),
    ('Ś', 68, &[0x301]),
    ('ś', 68, &[0x301]),
    ('Ŝ', 68, &[0x302]),
    ('ŝ', 68, &[0x302]),
    ('Ş', 68, &[0x327]),
    ('ş', 68, &[0x327]),
    ('Š', 68, &[0x30C]),
    ('š', 68, &[0x30C]),
    ('Ţ', 70, &[0x327]),
    ('ţ', 70, &[0x327]),
    ('Ť', 70, &[0x30C]),
    ('ť', 70, &[0x30C]),
    ('Ũ', 72, &[0x303]),
    ('ũ', 72, &[0x303]),
    ('Ū', 72, &[0x304]),
    ('ū', 72, &[0x304]),
    ('Ŭ', 72, &[0x306]),
    ('ŭ', 72, &[0x306]),
    ('Ů', 72, &[0x30A]),
    ('ů', 72, &[0x30A]),
    ('Ű', 72, &[0x30B]),
    ('ű', 72, &[0x30B]),
    ('Ų', 72, &[0x328]),
    ('ų', 72, &[0x328]),
    ('Ŵ', 74, &[0x302]),
    ('ŵ', 74, &[0x302]),
    ('Ŷ', 76, &[0x302]),
    ('ŷ', 76, &[0x302]),
    ('Ÿ', 76, &[0x308]),
    ('Ź', 77, &[0x301]),
    ('ź', 77, &[0x301]),
    ('Ż', 77, &[0x307]),
    ('ż', 77, &[0x307]),
    ('Ž', 77, &[0x30C]),
    ('ž', 77, &[0x30C]),
    ('Ơ', 62, &[0x31B]),
    ('ơ', 62, &[0x31B]),
    ('Ư', 72, &[0x31B]),
    ('ư', 72, &[0x31B]),
    ('Ǎ', 44, &[0x30C]),
    ('ǎ', 44, &[0x30C]),
    ('Ǐ', 53, &[0x30C]),
    ('ǐ', 53, &[0x30C]),
    ('Ǒ', 62, &[0x30C]),
    ('ǒ', 62, &[0x30C]),
    ('Ǔ', 72, &[0x30C]),
    ('ǔ', 72, &[0x30C]),
    ('Ǖ', 72, &[0x308, 0x304]),
    ('ǖ', 72, &[0x308, 0x304]),
    ('Ǘ', 72, &[0x308, 0x301]),
    ('ǘ', 72, &[0x308, 0x301]),
    ('Ǚ', 72, &[0x308, 0x30C]),
    ('ǚ', 72, &[0x308, 0x30C]),
    ('Ǜ', 72, &[0x308, 0x300]),
    ('ǜ', 72, &[0x308, 0x300]),
    ('Ǟ', 44, &[0x308, 0x304]),
    ('ǟ', 44, &[0x308, 0x304]),
    ('Ǡ', 44, &[0x307, 0x304]),
    ('ǡ', 44, &[0x307, 0x304]),
    ('Ǧ', 51, &[0x30C]),
    ('ǧ', 51, &[0x30C]),
    ('Ǩ', 57, &[0x30C]),
    ('ǩ', 57, &[0x30C]),
    ('Ǫ', 62, &[0x328]),
    ('ǫ', 62, &[0x328]),
    ('Ǭ', 62, &[0x328, 0x304]),
    ('ǭ', 62, &[0x328, 0x304]),
    ('ǰ', 56, &[0x30C]),
    ('Ǵ', 51, &[0x301]),
    ('ǵ', 51, &[0x301]),
    ('Ǹ', 60, &[0x300]),
    ('ǹ', 60, &[0x300]),
    ('Ǻ', 44, &[0x30A, 0x301]),
    ('ǻ', 44, &[0x30A, 0x301]),
    ('Ȁ', 44, &[0x30F]),
    ('ȁ', 44, &[0x30F]),
    ('Ȃ', 44, &[0x311]),
    ('ȃ', 44, &[0x311]),
    ('Ȅ', 49, &[0x30F]),
    ('ȅ', 49, &[0x30F]),
    ('Ȇ', 49, &[0x311]),
    ('ȇ', 49, &[0x311]),
    ('Ȉ', 53, &[0x30F]),
    ('ȉ', 53, &[0x30F]),
    ('Ȋ', 53, &[0x311]),
    ('ȋ', 53, &[0x311]),
    ('Ȍ', 62, &[0x30F]),
    ('ȍ', 62, &[0x30F]),
    ('Ȏ', 62, &[0x311]),
    ('ȏ', 62, &[0x311]),
    ('Ȑ', 67, &[0x30F]),
    ('ȑ', 67, &[0x30F]),
    ('Ȓ', 67, &[0x311]),
    ('ȓ', 67, &[0x311]),
    ('Ȕ', 72, &[0x30F]),
    ('ȕ', 72, &[0x30F]),
    ('Ȗ', 72, &[0x311]),
    ('ȗ', 72, &[0x311]),
    ('Ș', 68, &[0x326]),
    ('ș', 68, &[0x326]),
    ('Ț', 70, &[0x326]),
    ('ț', 70, &[0x326]),
    ('Ȟ', 52, &[0x30C]),
    ('ȟ', 52, &[0x30C]),
    ('Ȧ', 44, &[0x307]),
    ('ȧ', 44, &[0x307]),
    ('Ȩ', 49, &[0x327]),
    ('ȩ', 49, &[0x327]),
    ('Ȫ', 62, &[0x308, 0x304]),
    ('ȫ', 62, &[0x308, 0x304]),
    ('Ȭ', 62, &[0x303, 0x304]),
    ('ȭ', 62, &[0x303, 0x304]),
    ('Ȯ', 62, &[0x307]),
    ('ȯ', 62, &[0x307]),
    ('Ȱ', 62, &[0x307, 0x304]),
    ('ȱ', 62, &[0x307, 0x304]),
    ('Ȳ', 76, &[0x304]),
    ('ȳ', 76, &[0x304]),
    ('Ḃ', 46, &[0x307]),
    ('ḃ', 46, &[0x307]),
    ('Ḉ', 47, &[0x327, 0x301]),
    ('ḉ', 47, &[0x327, 0x301]),
    ('Ḋ', 48, &[0x307]),
    ('ḋ', 48, &[0x307]),
    ('Ḑ', 48, &[0x327]),
    ('ḑ', 48, &[0x327]),
    ('Ḕ', 49, &[0x304, 0x300]),
    ('ḕ', 49, &[0x304, 0x300]),
    ('Ḗ', 49, &[0x304, 0x301]),
    ('ḗ', 49, &[0x304, 0x301]),
    ('Ḝ', 49, &[0x327, 0x306]),
    ('ḝ', 49, &[0x327, 0x306]),
    ('Ḟ', 50, &[0x307]),
    ('ḟ', 50, &[0x307]),
    ('Ḡ', 51, &[0x304]),
    ('ḡ', 51, &[0x304]),
    ('Ḣ', 52, &[0x307]),
    ('ḣ', 52, &[0x307]),
    ('Ḧ', 52, &[0x308]),
    ('ḧ', 52, &[0x308]),
    ('Ḩ', 52, &[0x327]),
    ('ḩ', 52, &[0x327]),
    ('Ḯ', 53, &[0x308, 0x301]),
    ('ḯ', 53, &[0x308, 0x301]),
    ('Ḱ', 57, &[0x301]),
    ('ḱ', 57, &[0x301]),
    ('Ḿ', 59, &[0x301]),
    ('ḿ', 59, &[0x301]),
    ('Ṁ', 59, &[0x307]),
    ('ṁ', 59, &[0x307]),
    ('Ṅ', 60, &[0x307]),
    ('ṅ', 60, &[0x307]),
    ('Ṍ', 62, &[0x303, 0x301]),
    ('ṍ', 62, &[0x303, 0x301]),
    ('Ṏ', 62, &[0x303, 0x308]),
    ('ṏ', 62, &[0x303, 0x308]),
    ('Ṑ', 62, &[0x304, 0x300]),
    ('ṑ', 62, &[0x304, 0x300]),
    ('Ṓ', 62, &[0x304, 0x301]),
    ('ṓ', 62, &[0x304, 0x301]),
    ('Ṕ', 64, &[0x301]),
    ('ṕ', 64, &[0x301]),
    ('Ṗ', 64, &[0x307]),
    ('ṗ', 64, &[0x307]),
    ('Ṙ', 67, &[0x307]),
    ('ṙ', 67, &[0x307]),
    ('Ṡ', 68, &[0x307]),
    ('ṡ', 68, &[0x307]),
    ('Ṥ', 68, &[0x301, 0x307]),
    ('ṥ', 68, &[0x301, 0x307]),
    ('Ṧ', 68, &[0x30C, 0x307]),
    ('ṧ', 68, &[0x30C, 0x307]),
    ('Ṫ', 70, &[0x307]),
    ('ṫ', 70, &[0x307]),
    ('Ṹ', 72, &[0x303, 0x301]),
    ('ṹ', 72, &[0x303, 0x301]),
    ('Ṻ', 72, &[0x304, 0x308]),
    ('ṻ', 72, &[0x304, 0x308]),
    ('Ṽ', 73, &[0x303]),
    ('ṽ', 73, &[0x303]),
    ('Ẁ', 74, &[0x300]),
    ('ẁ', 74, &[0x300]),
    ('Ẃ', 74, &[0x301]),
    ('ẃ', 74, &[0x301]),
    ('Ẅ', 74, &[0x308]),
    ('ẅ', 74, &[0x308]),
    ('Ẇ', 74, &[0x307]),
    ('ẇ', 74, &[0x307]),
    ('Ẋ', 75, &[0x307]),
    ('ẋ', 75, &[0x307]),
    ('Ẍ', 75, &[0x308]),
    ('ẍ', 75, &[0x308]),
    ('Ẏ', 76, &[0x307]),
    ('ẏ', 76, &[0x307]),
    ('Ẑ', 77, &[0x302]),
    ('ẑ', 77, &[0x302]),
    ('ẗ', 70, &[0x308]),
    ('ẘ', 74, &[0x30A]),
    ('ẙ', 76, &[0x30A]),
    ('Ấ', 44, &[0x302, 0x301]),
    ('ấ', 44, &[0x302, 0x301]),
    ('Ầ', 44, &[0x302, 0x300]),
    ('ầ', 44, &[0x302, 0x300]),
    ('Ẫ', 44, &[0x302, 0x303]),
    ('ẫ', 44, &[0x302, 0x303]),
    ('Ắ', 44, &[0x306, 0x301]),
    ('ắ', 44, &[0x306, 0x301]),
    ('Ằ', 44, &[0x306, 0x300]),
    ('ằ', 44, &[0x306, 0x300]),
    ('Ẵ', 44, &[0x306, 0x303]),
    ('ẵ', 44, &[0x306, 0x303]),
    ('Ẽ', 49, &[0x303]),
    ('ẽ', 49, &[0x303]),
    ('Ế', 49, &[0x302, 0x301]),
    ('ế', 49, &[0x302, 0x301]),
    ('Ề', 49, &[0x302, 0x300]),
    ('ề', 49, &[0x302, 0x300]),
    ('Ễ', 49, &[0x302, 0x303]),
    ('ễ', 49, &[0x302, 0x303]),
    ('Ố', 62, &[0x302, 0x301]),
    ('ố', 62, &[0x302, 0x301]),
    ('Ồ', 62, &[0x302, 0x300]),
    ('ồ', 62, &[0x302, 0x300]),
    ('Ỗ', 62, &[0x302, 0x303]),
    ('ỗ', 62, &[0x302, 0x303]),
    ('Ớ', 62, &[0x31B, 0x301]),
    ('ớ', 62, &[0x31B, 0x301]),
    ('Ờ', 62, &[0x31B, 0x300]),
    ('ờ', 62, &[0x31B, 0x300]),
    ('Ỡ', 62, &[0x31B, 0x303]),
    ('ỡ', 62, &[0x31B, 0x303]),
    ('Ứ', 72, &[0x31B, 0x301]),
    ('ứ', 72, &[0x31B, 0x301]),
    ('Ừ', 72, &[0x31B, 0x300]),
    ('ừ', 72, &[0x31B, 0x300]),
    ('Ữ', 72, &[0x31B, 0x303]),
    ('ữ', 72, &[0x31B, 0x303]),
    ('Ỳ', 76, &[0x300]),
    ('ỳ', 76, &[0x300]),
    ('Ỹ', 76, &[0x303]),
    ('ỹ', 76, &[0x303]),
];

/// Measured primary weights for the remaining code points: `(start, end,
/// weight)`, sorted by start and disjoint.
const ICU_PRIMARY_RANGES: [(u32, u32, u8); 390] = [
    (0x180, 0x183, 46),
    (0x189, 0x18C, 48),
    (0x1AB, 0x1AE, 71),
    (0x1B5, 0x1BA, 77),
    (0x1C0, 0x1C3, 79),
    (0x1D3, 0x1DC, 72),
    (0x1DE, 0x1E1, 44),
    (0x1E4, 0x1E7, 51),
    (0x1EA, 0x1ED, 62),
    (0x200, 0x203, 44),
    (0x204, 0x207, 49),
    (0x208, 0x20B, 53),
    (0x20C, 0x20F, 62),
    (0x210, 0x213, 67),
    (0x214, 0x217, 72),
    (0x22A, 0x231, 62),
    (0x258, 0x25E, 49),
    (0x260, 0x263, 51),
    (0x26B, 0x26E, 58),
    (0x279, 0x281, 67),
    (0x290, 0x293, 77),
    (0x295, 0x298, 79),
    (0x2B3, 0x2B6, 67),
    (0x2C2, 0x2CF, 26),
    (0x2D2, 0x2DB, 26),
    (0x2E5, 0x2ED, 26),
    (0x2EF, 0x2FF, 26),
    (0x300, 0x362, 0),
    (0x390, 0x39B, 79),
    (0x39C, 0x3A1, 80),
    (0x3A3, 0x3A9, 80),
    (0x3AC, 0x3AF, 79),
    (0x3B1, 0x3BB, 79),
    (0x3BC, 0x3C9, 80),
    (0x3CB, 0x3CE, 80),
    (0x3D2, 0x3D6, 80),
    (0x3DA, 0x3DD, 79),
    (0x3DE, 0x3EF, 80),
    (0x3F7, 0x481, 80),
    (0x483, 0x489, 0),
    (0x48A, 0x52F, 80),
    (0x531, 0x556, 80),
    (0x560, 0x588, 80),
    (0x591, 0x5BD, 0),
    (0x5D0, 0x5EA, 80),
    (0x5EF, 0x5F2, 80),
    (0x610, 0x61A, 0),
    (0x620, 0x63F, 80),
    (0x641, 0x64A, 80),
    (0x64B, 0x65F, 0),
    (0x671, 0x6D3, 80),
    (0x6D6, 0x6DC, 0),
    (0x6DF, 0x6E4, 0),
    (0x6EA, 0x6ED, 0),
    (0x6FA, 0x6FF, 80),
    (0x703, 0x708, 6),
    (0x70A, 0x70D, 24),
    (0x712, 0x72F, 80),
    (0x730, 0x74A, 0),
    (0x74D, 0x7B1, 80),
    (0x7CA, 0x7EA, 80),
    (0x7EB, 0x7F3, 0),
    (0x800, 0x817, 80),
    (0x81C, 0x82D, 0),
    (0x830, 0x83E, 6),
    (0x840, 0x858, 80),
    (0x860, 0x86A, 80),
    (0x870, 0x887, 80),
    (0x889, 0x88F, 80),
    (0x897, 0x89F, 0),
    (0x8A0, 0x8C9, 80),
    (0x8CA, 0x8E1, 0),
    (0x8E3, 0x903, 0),
    (0x904, 0x93B, 80),
    (0x93D, 0x950, 80),
    (0x951, 0x954, 0),
    (0x955, 0x963, 80),
    (0x972, 0x980, 80),
    (0x985, 0x98C, 80),
    (0x993, 0x9A8, 80),
    (0x9AA, 0x9B0, 80),
    (0x9B6, 0x9B9, 80),
    (0x9BD, 0x9C4, 80),
    (0x9CB, 0x9CE, 80),
    (0x9DF, 0x9E3, 80),
    (0x9F4, 0x9F9, 43),
    (0xA05, 0xA0A, 80),
    (0xA13, 0xA28, 80),
    (0xA2A, 0xA30, 80),
    (0xA3E, 0xA42, 80),
    (0xA59, 0xA5C, 80),
    (0xA72, 0xA75, 80),
    (0xA85, 0xA8D, 80),
    (0xA93, 0xAA8, 80),
    (0xAAA, 0xAB0, 80),
    (0xAB5, 0xAB9, 80),
    (0xABD, 0xAC5, 80),
    (0xAE0, 0xAE3, 80),
    (0xAFA, 0xAFF, 0),
    (0xB05, 0xB0C, 80),
    (0xB13, 0xB28, 80),
    (0xB2A, 0xB30, 80),
    (0xB35, 0xB39, 80),
    (0xB3D, 0xB44, 80),
    (0xB5F, 0xB63, 80),
    (0xB72, 0xB77, 43),
    (0xB85, 0xB8A, 80),
    (0xB92, 0xB95, 80),
    (0xBAE, 0xBB9, 80),
    (0xBBE, 0xBC2, 80),
    (0xBCA, 0xBCD, 80),
    (0xBEF, 0xBF2, 43),
    (0xBF3, 0xBF8, 26),
    (0xC00, 0xC04, 0),
    (0xC05, 0xC0C, 80),
    (0xC12, 0xC28, 80),
    (0xC2A, 0xC39, 80),
    (0xC3D, 0xC44, 80),
    (0xC4A, 0xC4D, 80),
    (0xC60, 0xC63, 80),
    (0xC85, 0xC8C, 80),
    (0xC92, 0xCA8, 80),
    (0xCAA, 0xCB3, 80),
    (0xCB5, 0xCB9, 80),
    (0xCBD, 0xCC4, 80),
    (0xCCA, 0xCCD, 80),
    (0xCE0, 0xCE3, 80),
    (0xD00, 0xD03, 0),
    (0xD04, 0xD0C, 80),
    (0xD12, 0xD44, 80),
    (0xD4A, 0xD4E, 80),
    (0xD54, 0xD57, 80),
    (0xD58, 0xD5E, 43),
    (0xD5F, 0xD63, 80),
    (0xD6F, 0xD78, 43),
    (0xD7A, 0xD7F, 80),
    (0xD85, 0xD96, 80),
    (0xD9A, 0xDB1, 80),
    (0xDB3, 0xDBB, 80),
    (0xDC0, 0xDC6, 80),
    (0xDCF, 0xDD4, 80),
    (0xDD8, 0xDDF, 80),
    (0xE01, 0xE3A, 80),
    (0xE40, 0xE45, 80),
    (0xE47, 0xE4E, 0),
    (0xE86, 0xE8A, 80),
    (0xE8C, 0xEA3, 80),
    (0xEA7, 0xEBD, 80),
    (0xEC0, 0xEC4, 80),
    (0xEC8, 0xECE, 0),
    (0xEDC, 0xEDF, 80),
    (0xF04, 0xF12, 24),
    (0xF1A, 0xF1F, 26),
    (0xF3A, 0xF3D, 17),
    (0xF40, 0xF47, 80),
    (0xF49, 0xF6C, 80),
    (0xF71, 0xF7D, 80),
    (0xF88, 0xF97, 80),
    (0xF99, 0xFBC, 80),
    (0xFBE, 0xFC5, 26),
    (0xFC7, 0xFCC, 26),
    (0xFD0, 0xFD4, 24),
    (0xFD5, 0xFD8, 26),
    (0x1000, 0x1035, 80),
    (0x1039, 0x103F, 80),
    (0x104C, 0x104F, 24),
    (0x1050, 0x108F, 80),
    (0x109A, 0x109D, 80),
    (0x10A0, 0x10C5, 80),
    (0x10D0, 0x10FA, 80),
    (0x10FC, 0x1248, 80),
    (0x124A, 0x124D, 80),
    (0x1250, 0x1256, 80),
    (0x125A, 0x125D, 80),
    (0x1260, 0x1288, 80),
    (0x128A, 0x128D, 80),
    (0x1290, 0x12B0, 80),
    (0x12B2, 0x12B5, 80),
    (0x12B8, 0x12BE, 80),
    (0x12C2, 0x12C5, 80),
    (0x12C8, 0x12D6, 80),
    (0x12D8, 0x1310, 80),
    (0x1312, 0x1315, 80),
    (0x1318, 0x135A, 80),
    (0x1363, 0x1366, 6),
    (0x1371, 0x137C, 43),
    (0x1380, 0x138F, 80),
    (0x1390, 0x1399, 26),
    (0x13A0, 0x13F5, 80),
    (0x13F8, 0x13FD, 80),
    (0x1401, 0x166C, 80),
    (0x166F, 0x167F, 80),
    (0x1681, 0x169A, 80),
    (0x16A0, 0x16EA, 80),
    (0x16EE, 0x16F8, 80),
    (0x1700, 0x1715, 80),
    (0x171F, 0x1734, 80),
    (0x1740, 0x1753, 80),
    (0x1760, 0x176C, 80),
    (0x1780, 0x17B3, 80),
    (0x17B6, 0x17C5, 80),
    (0x17C6, 0x17D1, 0),
    (0x180A, 0x180D, 0),
    (0x1820, 0x1878, 80),
    (0x1880, 0x18AA, 80),
    (0x18B0, 0x18F5, 80),
    (0x1900, 0x191E, 80),
    (0x1920, 0x192B, 80),
    (0x1930, 0x1938, 80),
    (0x1950, 0x196D, 80),
    (0x1970, 0x1974, 80),
    (0x1980, 0x19AB, 80),
    (0x19B0, 0x19C9, 80),
    (0x19E0, 0x19FF, 26),
    (0x1A00, 0x1A1B, 80),
    (0x1A20, 0x1A5E, 80),
    (0x1A60, 0x1A73, 80),
    (0x1A74, 0x1A7C, 0),
    (0x1AA0, 0x1AA6, 24),
    (0x1AA8, 0x1AAB, 9),
    (0x1AB0, 0x1ABE, 0),
    (0x1AC1, 0x1ACB, 0),
    (0x1ACF, 0x1ADD, 0),
    (0x1AE0, 0x1AEB, 0),
    (0x1B00, 0x1B04, 0),
    (0x1B05, 0x1B33, 80),
    (0x1B35, 0x1B4C, 80),
    (0x1B61, 0x1B6A, 26),
    (0x1B6B, 0x1B73, 0),
    (0x1B74, 0x1B7C, 26),
    (0x1B83, 0x1BAF, 80),
    (0x1BBA, 0x1BE5, 80),
    (0x1BE7, 0x1BF3, 80),
    (0x1BFC, 0x1BFF, 24),
    (0x1C00, 0x1C36, 80),
    (0x1C5A, 0x1C7D, 80),
    (0x1C80, 0x1C8A, 80),
    (0x1C90, 0x1CBA, 80),
    (0x1CC0, 0x1CC7, 24),
    (0x1CD0, 0x1CE8, 0),
    (0x1CE9, 0x1CEC, 80),
    (0x1CEE, 0x1CF1, 80),
    (0x1D0F, 0x1D17, 63),
    (0x1D1C, 0x1D1F, 72),
    (0x1D24, 0x1D27, 79),
    (0x1D28, 0x1D2B, 80),
    (0x1D49, 0x1D4C, 49),
    (0x1D5C, 0x1D5F, 79),
    (0x1D92, 0x1D95, 49),
    (0x1DA4, 0x1DA7, 55),
    (0x1DBB, 0x1DBE, 77),
    (0x1DC0, 0x1DC9, 0),
    (0x1DCB, 0x1DD1, 0),
    (0x1DF5, 0x1DFF, 0),
    (0x1E02, 0x1E07, 46),
    (0x1E0A, 0x1E13, 48),
    (0x1E14, 0x1E1D, 49),
    (0x1E22, 0x1E2B, 52),
    (0x1E2C, 0x1E2F, 53),
    (0x1E30, 0x1E35, 57),
    (0x1E36, 0x1E3D, 58),
    (0x1E3E, 0x1E43, 59),
    (0x1E44, 0x1E4B, 60),
    (0x1E4C, 0x1E53, 62),
    (0x1E54, 0x1E57, 64),
    (0x1E58, 0x1E5F, 67),
    (0x1E60, 0x1E69, 68),
    (0x1E6A, 0x1E71, 70),
    (0x1E72, 0x1E7B, 72),
    (0x1E7C, 0x1E7F, 73),
    (0x1E80, 0x1E89, 74),
    (0x1E8A, 0x1E8D, 75),
    (0x1E90, 0x1E95, 77),
    (0x1EA0, 0x1EB7, 44),
    (0x1EB8, 0x1EC7, 49),
    (0x1EC8, 0x1ECB, 53),
    (0x1ECC, 0x1EE3, 62),
    (0x1EE4, 0x1EF1, 72),
    (0x1EF2, 0x1EF9, 76),
    (0x1F00, 0x1F15, 79),
    (0x1F18, 0x1F1D, 79),
    (0x1F20, 0x1F3F, 79),
    (0x1F40, 0x1F45, 80),
    (0x1F48, 0x1F4D, 80),
    (0x1F50, 0x1F57, 80),
    (0x1F5F, 0x1F6F, 80),
    (0x1F70, 0x1F77, 79),
    (0x1F78, 0x1F7D, 80),
    (0x1F80, 0x1F9F, 79),
    (0x1FA0, 0x1FAF, 80),
    (0x1FB0, 0x1FB4, 79),
    (0x1FB6, 0x1FBC, 79),
    (0x1FC6, 0x1FCC, 79),
    (0x1FD0, 0x1FD3, 79),
    (0x1FD6, 0x1FDB, 79),
    (0x1FE0, 0x1FEC, 80),
    (0x1FF6, 0x1FFC, 80),
    (0x2000, 0x200A, 1),
    (0x2010, 0x2015, 3),
    (0x2018, 0x201B, 10),
    (0x201C, 0x201F, 11),
    (0x2020, 0x2023, 24),
    (0x2030, 0x2038, 24),
    (0x203F, 0x2043, 24),
    (0x2058, 0x205E, 9),
    (0x20A0, 0x20A6, 33),
    (0x20A9, 0x20C1, 33),
    (0x20D0, 0x20F0, 0),
    (0x210B, 0x210F, 52),
    (0x2135, 0x2138, 80),
    (0x2140, 0x2144, 26),
    (0x2150, 0x2153, 35),
    (0x2164, 0x2167, 73),
    (0x2174, 0x2177, 73),
    (0x2190, 0x2211, 26),
    (0x2308, 0x230B, 17),
    (0x2469, 0x2472, 35),
    (0x2474, 0x2487, 12),
    (0x2491, 0x249A, 35),
    (0x249C, 0x24B5, 12),
    (0x24EB, 0x24F3, 35),
    (0x2768, 0x2775, 17),
    (0x27E6, 0x27EF, 17),
    (0x2983, 0x2998, 17),
    (0x29D8, 0x29DB, 17),
    (0x2E80, 0x2E99, 80),
    (0x2E9B, 0x2EF3, 80),
    (0x2F00, 0x2FD5, 80),
    (0x3008, 0x3011, 17),
    (0x3014, 0x301B, 17),
    (0x302A, 0x302F, 0),
    (0x3031, 0x3037, 32),
    (0x3041, 0x3096, 80),
    (0x30A1, 0x30FA, 80),
    (0x31F0, 0x31FF, 80),
    (0x3300, 0x3357, 80),
    (0x3362, 0x336B, 35),
    (0x336C, 0x3370, 36),
    (0x337B, 0x337F, 80),
    (0x33C4, 0x33C7, 47),
    (0x33D0, 0x33D3, 58),
    (0x33D7, 0x33DA, 64),
    (0x33E9, 0x33F2, 35),
    (0x33F3, 0x33FC, 36),
    (0x4E00, 0x9FFF, 80),
    (0xAC00, 0xD7A3, 80),
    (0xF900, 0xFA6D, 80),
    (0xFA70, 0xFAD9, 80),
    (0xFE38, 0xFE44, 17),
    (0xFE49, 0xFE4C, 1),
    (0xFE70, 0xFE74, 0),
    (0xFE76, 0xFE7F, 0),
    (0xFE80, 0xFEFC, 80),
    (0x1D165, 0x1D169, 0),
    (0x1D16D, 0x1D172, 0),
    (0x1D17B, 0x1D182, 0),
    (0x1D185, 0x1D18B, 0),
    (0x1D1AA, 0x1D1AD, 0),
    (0x1D2C9, 0x1D2D3, 43),
    (0x1D2E9, 0x1D2F3, 43),
    (0x1D368, 0x1D371, 43),
    (0x1D6A8, 0x1D6B2, 79),
    (0x1D6B3, 0x1D6B8, 80),
    (0x1D6BA, 0x1D6C0, 80),
    (0x1D6C2, 0x1D6CC, 79),
    (0x1D6CD, 0x1D6DA, 80),
    (0x1D6E2, 0x1D6EC, 79),
    (0x1D6ED, 0x1D6F2, 80),
    (0x1D6F4, 0x1D6FA, 80),
    (0x1D6FC, 0x1D706, 79),
    (0x1D707, 0x1D714, 80),
    (0x1D71C, 0x1D726, 79),
    (0x1D727, 0x1D72C, 80),
    (0x1D72E, 0x1D734, 80),
    (0x1D736, 0x1D740, 79),
    (0x1D741, 0x1D74E, 80),
    (0x1D756, 0x1D760, 79),
    (0x1D761, 0x1D766, 80),
    (0x1D768, 0x1D76E, 80),
    (0x1D770, 0x1D77A, 79),
    (0x1D77B, 0x1D788, 80),
    (0x1D790, 0x1D79A, 79),
    (0x1D79B, 0x1D7A0, 80),
    (0x1D7A2, 0x1D7A8, 80),
    (0x1D7AA, 0x1D7B4, 79),
    (0x1D7B5, 0x1D7C2, 80),
    (0x1F110, 0x1F129, 12),
    (0x1F210, 0x1F23B, 80),
    (0x1F240, 0x1F248, 17),
    (0x20000, 0x2A6DF, 80),
];

// ---------------------------------------------------------------------------
// `localeCompare`
//
// `getFileSuggestions` breaks ties with `a.label.localeCompare(b.label)`
// (packages/tui/src/autocomplete.ts:666), i.e. the runtime's default
// `Intl.Collator` (ICU root/en-US). That comparison resolves three levels -
// primary (base letters, digits, punctuation), secondary (accents) and tertiary
// (case) - before falling back to code points. A byte `cmp` orders
// punctuation-first names and case pairs differently (ICU: `.git` < `_x`,
// `apple.txt` < `README.md`), so the port compares the same three levels.
// ---------------------------------------------------------------------------

/// Binary search in `ICU_PRIMARY_CHARS`.
fn icu_char_weight(ch: char) -> Option<u8> {
    let code = ch as u32;
    let mut low = 0usize;
    let mut high = ICU_PRIMARY_CHARS.len();
    while low < high {
        let mid = low + (high - low) / 2;
        let (current, weight) = ICU_PRIMARY_CHARS[mid];
        let current_code = current as u32;
        if code < current_code {
            high = mid;
        } else if code > current_code {
            low = mid + 1;
        } else {
            return Some(weight);
        }
    }
    None
}

/// Binary search in `ICU_LATIN_FOLDS`.
fn icu_latin_fold(ch: char) -> Option<(u8, &'static [u32])> {
    let code = ch as u32;
    let mut low = 0usize;
    let mut high = ICU_LATIN_FOLDS.len();
    while low < high {
        let mid = low + (high - low) / 2;
        let (current, weight, marks) = ICU_LATIN_FOLDS[mid];
        let current_code = current as u32;
        if code < current_code {
            high = mid;
        } else if code > current_code {
            low = mid + 1;
        } else {
            return Some((weight, marks));
        }
    }
    None
}

/// Rank of a combining mark on the ICU secondary level.
fn icu_mark_rank(mark: u32) -> u32 {
    let mut index = 0usize;
    while index < ICU_MARK_ORDER.len() {
        if ICU_MARK_ORDER[index] == mark {
            return index as u32 + 1;
        }
        index += 1;
    }
    // Marks outside the measured set keep their relative code-point order.
    ICU_MARK_ORDER.len() as u32 + 2 + (mark & 0xFF)
}

/// Binary search in `ICU_PRIMARY_RANGES`.
fn icu_range_weight(ch: char) -> Option<u8> {
    let code = ch as u32;
    let mut low = 0usize;
    let mut high = ICU_PRIMARY_RANGES.len();
    while low < high {
        let mid = low + (high - low) / 2;
        let (start, end, weight) = ICU_PRIMARY_RANGES[mid];
        if code < start {
            high = mid;
        } else if code > end {
            low = mid + 1;
        } else {
            return Some(weight);
        }
    }
    None
}

/// One code point's contribution to the primary and secondary levels, plus
/// whether it is a ligature expansion (`ß` -> `ss`) that ICU compares first.
fn icu_char_levels(ch: char) -> (Vec<u32>, Vec<u32>, bool) {
    let expansion: Option<&str> = match ch {
        'ß' => Some("ss"),
        'æ' | 'Æ' => Some("ae"),
        'œ' | 'Œ' => Some("oe"),
        _ => None,
    };
    if let Some(expansion) = expansion {
        let primary = expansion
            .chars()
            .map(|part| u32::from(icu_char_weight(part).unwrap_or(0)))
            .collect::<Vec<u32>>();
        return (primary, Vec::new(), true);
    }

    let lower = ch.to_lowercase().next().unwrap_or(ch);
    let fold = icu_latin_fold(ch).or_else(|| icu_latin_fold(lower));
    if let Some((weight, marks)) = fold {
        let secondary = marks.iter().map(|mark| icu_mark_rank(*mark)).collect::<Vec<u32>>();
        return (vec![u32::from(weight)], secondary, false);
    }
    if let Some(weight) = icu_char_weight(ch) {
        return (vec![u32::from(weight)], Vec::new(), false);
    }
    if let Some(weight) = icu_range_weight(lower) {
        return (vec![u32::from(weight)], Vec::new(), false);
    }
    // Long tail: letters sort with the non-Latin scripts ICU places after Latin,
    // other code points keep a symbol slot; the code point breaks further ties.
    let weight = if lower.is_alphabetic() { 80u32 } else { 32u32 };
    (vec![weight], Vec::new(), false)
}

/// Port of `String#localeCompare` with the default locale.
fn locale_compare(left: &str, right: &str) -> std::cmp::Ordering {
    if left == right {
        return std::cmp::Ordering::Equal;
    }

    let mut left_primary: Vec<u32> = Vec::new();
    let mut left_secondary: Vec<u32> = Vec::new();
    let mut left_expansions: Vec<bool> = Vec::new();
    for ch in left.chars() {
        let (primary, secondary, expansion) = icu_char_levels(ch);
        left_primary.extend(primary);
        left_secondary.extend(secondary);
        left_expansions.push(expansion);
    }

    let mut right_primary: Vec<u32> = Vec::new();
    let mut right_secondary: Vec<u32> = Vec::new();
    let mut right_expansions: Vec<bool> = Vec::new();
    for ch in right.chars() {
        let (primary, secondary, expansion) = icu_char_levels(ch);
        right_primary.extend(primary);
        right_secondary.extend(secondary);
        right_expansions.push(expansion);
    }

    if left_primary != right_primary {
        return left_primary.cmp(&right_primary);
    }
    if left_secondary != right_secondary {
        return left_secondary.cmp(&right_secondary);
    }
    if left_expansions != right_expansions {
        return left_expansions.cmp(&right_expansions);
    }

    // Tertiary level: within a primary group, lowercase precedes uppercase.
    let left_tertiary: Vec<bool> = left.chars().map(char::is_uppercase).collect();
    let right_tertiary: Vec<bool> = right.chars().map(char::is_uppercase).collect();
    if left_tertiary != right_tertiary {
        return left_tertiary.cmp(&right_tertiary);
    }

    left.cmp(right)
}

/// Port of `path.join` - `path.win32.join` when running on Windows
/// (`autocomplete.ts:4` imports `join` from `path`).
fn join_path(base: &str, part: &str) -> String {
    if cfg!(windows) {
        return win32_join(&[base, part]);
    }
    posix_join(&[base, part])
}

/// Port of `path.posix.join`.
fn posix_join(args: &[&str]) -> String {
    let parts: Vec<&str> = args.iter().copied().filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        return ".".to_string();
    }
    posix_normalize(&parts.join("/"))
}

/// Port of `path.win32.join`.
fn win32_join(args: &[&str]) -> String {
    let parts: Vec<&str> = args.iter().copied().filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        return ".".to_string();
    }

    let first_chars: Vec<char> = parts[0].chars().collect();
    let mut joined = parts.join("\\");

    // Avoid leading `\\` being mistaken for a UNC root by `normalize`.
    let mut needs_replace = true;
    let mut slash_count = 0usize;
    if first_chars.first().copied().map(is_path_separator).unwrap_or(false) {
        slash_count += 1;
        if first_chars.len() > 1 && is_path_separator(first_chars[1]) {
            slash_count += 1;
            if first_chars.len() > 2 {
                if is_path_separator(first_chars[2]) {
                    slash_count += 1;
                } else {
                    needs_replace = false;
                }
            }
        }
    }
    if needs_replace {
        let joined_chars: Vec<char> = joined.chars().collect();
        while slash_count < joined_chars.len() && is_path_separator(joined_chars[slash_count]) {
            slash_count += 1;
        }
        if slash_count >= 2 {
            joined = format!("\\{}", joined_chars[slash_count..].iter().collect::<String>());
        }
    }

    // Reserved device names skip normalization, but `/` becomes `\`.
    let mut has_reserved_segment = false;
    for segment in joined.split('\\').filter(|segment| !segment.is_empty()) {
        if let Some(colon) = segment.find(':') {
            if is_windows_reserved_name(segment, Some(colon)) {
                has_reserved_segment = true;
                break;
            }
        }
    }
    if has_reserved_segment {
        return joined.chars().map(|ch| if ch == '/' { '\\' } else { ch }).collect();
    }

    win32_normalize(&joined)
}

/// Port of `path.posix.normalize`.
fn posix_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let chars: Vec<char> = path.chars().collect();
    let is_absolute = chars[0] == '/';
    let trailing_separator = chars[chars.len() - 1] == '/';
    let mut normalized = normalize_string(path, !is_absolute, '/', true);
    if normalized.is_empty() {
        if is_absolute {
            return "/".to_string();
        }
        return if trailing_separator { "./".to_string() } else { ".".to_string() };
    }
    if trailing_separator {
        normalized.push('/');
    }
    if is_absolute {
        format!("/{normalized}")
    } else {
        normalized
    }
}

/// Port of `path.win32.normalize`.
fn win32_normalize(path: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    let length = chars.len();
    if length == 0 {
        return ".".to_string();
    }
    let mut root_end = 0usize;
    let mut device: Option<String> = None;
    let mut is_absolute = false;
    let code = chars[0];
    let colon_index = chars.iter().position(|ch| *ch == ':');

    if length == 1 {
        return if is_path_separator(code) && code == '/' {
            "\\".to_string()
        } else {
            path.to_string()
        };
    }
    if is_path_separator(code) {
        // Possible UNC root.
        is_absolute = true;
        if is_path_separator(chars[1]) {
            let mut j = 2usize;
            let mut last = j;
            while j < length && !is_path_separator(chars[j]) {
                j += 1;
            }
            if j < length && j != last {
                let first_part: String = chars[last..j].iter().collect();
                last = j;
                while j < length && is_path_separator(chars[j]) {
                    j += 1;
                }
                if j < length && j != last {
                    last = j;
                    while j < length && !is_path_separator(chars[j]) {
                        j += 1;
                    }
                    if j == length || j != last {
                        if first_part == "." || first_part == "?" {
                            device = Some(format!("\\\\{first_part}"));
                            root_end = 4;
                            let possible_device: String = match colon_index {
                                Some(colon) if colon + 1 >= 4 && colon + 1 <= length => {
                                    chars[4..=colon].iter().collect()
                                }
                                _ => String::new(),
                            };
                            if is_windows_reserved_name(&possible_device, possible_device.chars().count().checked_sub(1)) {
                                device = Some(format!("\\\\?\\{possible_device}"));
                                root_end = 4 + possible_device.chars().count();
                            }
                        } else if j == length {
                            return format!("\\\\{first_part}\\{}\\", chars[last..].iter().collect::<String>());
                        } else {
                            device = Some(format!(
                                "\\\\{first_part}\\{}",
                                chars[last..j].iter().collect::<String>()
                            ));
                            root_end = j;
                        }
                    }
                }
            }
        } else {
            root_end = 1;
        }
    } else if let Some(colon) = colon_index {
        if colon > 0 {
            if code.is_ascii_alphabetic() && colon == 1 {
                device = Some(chars[0..2].iter().collect());
                root_end = 2;
                if length > 2 && is_path_separator(chars[2]) {
                    is_absolute = true;
                    root_end = 3;
                }
            } else if is_windows_reserved_name(path, Some(colon)) {
                device = Some(chars[..=colon].iter().collect());
                root_end = colon + 1;
            }
        }
    }

    let mut tail = if root_end < length {
        normalize_string(&chars[root_end..].iter().collect::<String>(), !is_absolute, '\\', false)
    } else {
        String::new()
    };
    if tail.is_empty() && !is_absolute {
        tail = ".".to_string();
    }
    if !tail.is_empty() && is_path_separator(chars[length - 1]) {
        tail.push('\\');
    }
    if !is_absolute && device.is_none() && chars.contains(&':') {
        // Ensure the tail is not read by Windows as an absolute path
        // (CVE-2024-36139).
        let tail_chars: Vec<char> = tail.chars().collect();
        if tail_chars.len() >= 2 && tail_chars[0].is_ascii_alphabetic() && tail_chars[1] == ':' {
            return format!(".\\{tail}");
        }
        let mut index = colon_index;
        while let Some(current) = index {
            if current == length - 1 || is_path_separator(chars[current + 1]) {
                return format!(".\\{tail}");
            }
            index = chars[current + 1..]
                .iter()
                .position(|ch| *ch == ':')
                .map(|next| current + 1 + next);
        }
    }
    if is_windows_reserved_name(path, colon_index) {
        return format!(".\\{}{tail}", device.unwrap_or_default());
    }
    match device {
        None => {
            if is_absolute {
                format!("\\{tail}")
            } else {
                tail
            }
        }
        Some(device) => {
            if is_absolute {
                format!("{device}\\{tail}")
            } else {
                format!("{device}{tail}")
            }
        }
    }
}

/// Port of `path.dirname` - `path.win32.dirname` on Windows.
fn dirname(path: &str) -> String {
    if cfg!(windows) {
        return win32_dirname(path);
    }
    let chars: Vec<char> = path.chars().collect();
    let length = chars.len() as i64;
    if length == 0 {
        return ".".to_string();
    }
    let has_root = chars[0] == '/';
    let mut end: i64 = -1;
    let mut matched_slash = true;
    let mut i = length - 1;
    while i >= 1 {
        if chars[i as usize] == '/' {
            if !matched_slash {
                end = i;
                break;
            }
        } else {
            matched_slash = false;
        }
        i -= 1;
    }
    if end == -1 {
        return if has_root { "/".to_string() } else { ".".to_string() };
    }
    if has_root && end == 1 {
        return "//".to_string();
    }
    chars[..end as usize].iter().collect()
}

/// Port of `path.win32.dirname`.
fn win32_dirname(path: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    let length = chars.len();
    if length == 0 {
        return ".".to_string();
    }
    let mut root_end: i64 = -1;
    let mut offset = 0usize;
    let code = chars[0];

    if length == 1 {
        return if is_path_separator(code) { path.to_string() } else { ".".to_string() };
    }
    if is_path_separator(code) {
        // Possible UNC root.
        root_end = 1;
        offset = 1;
        if is_path_separator(chars[1]) {
            let mut j = 2usize;
            let mut last = j;
            while j < length && !is_path_separator(chars[j]) {
                j += 1;
            }
            if j < length && j != last {
                last = j;
                while j < length && is_path_separator(chars[j]) {
                    j += 1;
                }
                if j < length && j != last {
                    last = j;
                    while j < length && !is_path_separator(chars[j]) {
                        j += 1;
                    }
                    if j == length {
                        return path.to_string();
                    }
                    if j != last {
                        root_end = j as i64 + 1;
                        offset = j + 1;
                    }
                }
            }
        }
    } else if code.is_ascii_alphabetic() && chars[1] == ':' {
        root_end = if length > 2 && is_path_separator(chars[2]) { 3 } else { 2 };
        offset = root_end as usize;
    }

    let mut end: i64 = -1;
    let mut matched_slash = true;
    let mut i = length as i64 - 1;
    while i >= offset as i64 {
        if is_path_separator(chars[i as usize]) {
            if !matched_slash {
                end = i;
                break;
            }
        } else {
            matched_slash = false;
        }
        i -= 1;
    }

    if end == -1 {
        if root_end == -1 {
            return ".".to_string();
        }
        end = root_end;
    }
    chars[..end as usize].iter().collect()
}

/// Port of `path.basename` - `path.win32.basename` on Windows.
fn basename(path: &str) -> String {
    if cfg!(windows) {
        return win32_basename(path);
    }
    let chars: Vec<char> = path.chars().collect();
    let length = chars.len() as i64;
    let mut start = 0usize;
    let mut end: i64 = -1;
    let mut matched_slash = true;
    let mut i = length - 1;
    while i >= 0 {
        if chars[i as usize] == '/' {
            if !matched_slash {
                start = i as usize + 1;
                break;
            }
        } else if end == -1 {
            matched_slash = false;
            end = i + 1;
        }
        i -= 1;
    }
    if end == -1 {
        return String::new();
    }
    chars[start..end as usize].iter().collect()
}

/// Port of `path.win32.basename`.
fn win32_basename(path: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    let length = chars.len();
    let mut start = 0usize;
    let mut end: i64 = -1;
    let mut matched_slash = true;
    if length >= 2 && chars[0].is_ascii_alphabetic() && chars[1] == ':' {
        start = 2;
    }
    let mut i = length as i64 - 1;
    while i >= start as i64 {
        if is_path_separator(chars[i as usize]) {
            if !matched_slash {
                start = i as usize + 1;
                break;
            }
        } else if end == -1 {
            matched_slash = false;
            end = i + 1;
        }
        i -= 1;
    }
    if end == -1 {
        return String::new();
    }
    chars[start..end as usize].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The healthy path must still read the child's stdout and exit status after
    /// the polled wait replaced `wait_with_output`.
    #[test]
    fn fd_walk_reads_stdout_from_a_completed_child() {
        let dir = tempfile::tempdir().expect("temp dir");
        #[cfg(windows)]
        let script = {
            let path = dir.path().join("fake-fd.cmd");
            std::fs::write(&path, "@echo off\necho src/foo.rs\necho src/sub/\n").unwrap();
            path
        };
        #[cfg(not(windows))]
        let script = {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.path().join("fake-fd.sh");
            std::fs::write(&path, "#!/bin/sh\necho src/foo.rs\necho src/sub/\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };

        let signal = AbortSignal::new();
        let entries = walk_directory_with_fd(".", &script.to_string_lossy(), "", 100, &signal);
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        // TS splits the output on "\n" only (autocomplete.ts:198), so a child
        // that emits CRLF keeps the "\r" on the line. The port matches that.
        let trailing = if cfg!(windows) { "\r" } else { "" };
        assert_eq!(paths, vec![format!("src/foo.rs{trailing}").as_str(), "src/sub/"]);
        assert_eq!(entries[0].is_directory, false);
        assert_eq!(entries[1].is_directory, true);
    }

    /// End-to-end TUIR-9: on Windows, a backslash path prefix typed in the editor
    /// (Tab passes `force`) must list the entries under that directory, exactly
    /// as the platform `path` helpers allow (`autocomplete.ts:587-594`).
    #[tokio::test]
    async fn backslash_prefix_lists_directory_entries_end_to_end() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("foo.txt"), "x").unwrap();
        std::fs::write(dir.path().join("src").join("foobar.txt"), "x").unwrap();

        let provider = CombinedAutocompleteProvider::new(Vec::new(), &dir.path().to_string_lossy(), None);
        let signal = AbortSignal::new();

        #[cfg(windows)]
        let line = "src\\fo".to_string();
        #[cfg(not(windows))]
        let line = "src/fo".to_string();

        let suggestions = provider
            .get_suggestions(&[line.clone()], 0, line.chars().count(), &signal, true)
            .await
            .expect("backslash prefix must resolve to the src directory");
        let values: Vec<&str> = suggestions.items.iter().map(|item| item.value.as_str()).collect();
        assert_eq!(suggestions.prefix, line);
        assert_eq!(suggestions.kind.as_deref(), Some("file"));
        assert!(values.contains(&"src/foo.txt"), "got {values:?}");
        assert!(values.contains(&"src/foobar.txt"), "got {values:?}");
    }

    // --- TUIR-11 regression: the `fd` child dies when the signal aborts -------

    /// TS kills the spawned `fd` child on abort (`child.kill("SIGKILL")`,
    /// autocomplete.ts:178-184) and resolves `[]`. Assert that an aborted walk
    /// returns immediately with no entries instead of blocking until `fd` exits.
    #[test]
    fn aborted_fd_walk_kills_the_child_and_returns_empty() {
        let shell = if cfg!(windows) { "cmd.exe" } else { "/bin/sh" };
        let arguments: Vec<&str> = if cfg!(windows) { vec!["/c", "ping -n 30 127.0.0.1"] } else { vec!["-c", "sleep 30"] };
        let mut child = std::process::Command::new(shell)
            .args(&arguments)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn long-running child");

        let signal = AbortSignal::new();
        signal.abort();
        let started = std::time::Instant::now();
        let aborted = wait_for_child_or_abort(&mut child, &signal);
        assert!(aborted, "aborting the signal must end the wait");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the watcher must not block until the child exits"
        );
        let exited = matches!(child.try_wait(), Ok(Some(_)));
        assert!(exited, "the child must be reaped after the abort");
    }

    /// A walk that is already aborted must not spawn anything.
    #[test]
    fn aborted_fd_walk_returns_no_entries() {
        let signal = AbortSignal::new();
        signal.abort();
        let entries = walk_directory_with_fd(".", "definitely-not-fd", "", 100, &signal);
        assert!(entries.is_empty());
    }

    // --- TUIR-8 regressions: `localeCompare` order (autocomplete.ts:666) ------

    /// TS orders file-suggestion ties with `a.label.localeCompare(b.label)`
    /// (autocomplete.ts:666). A byte `cmp` inverts case pairs and
    /// punctuation-first names, so assert the ICU result for the audit's example.
    #[test]
    fn locale_compare_orders_ties_like_the_runtime() {
        assert_eq!(locale_compare("apple.txt", "README.md"), std::cmp::Ordering::Less);
        assert_eq!(locale_compare("README.md", "apple.txt"), std::cmp::Ordering::Greater);
        assert_eq!(locale_compare("same", "same"), std::cmp::Ordering::Equal);
    }

    /// ICU primary order puts punctuation before digits before letters, and
    /// lowercase before the matching uppercase (`path.win32` names are ASCII).
    #[test]
    fn locale_compare_primary_order_matches_icu() {
        // Measured: " " < "_" < "-" < "!" < "." < "~" < "$" < digits < letters.
        let ordered = ["_x", "-y", "!bang", ".git", "~bak", "0start", "a.txt", "apple.txt", "B.txt", "Zebra"];
        let mut sorted = ordered.to_vec();
        sorted.sort_by(|left, right| locale_compare(left, right));
        assert_eq!(sorted, ordered);
    }

    /// Measured ICU accent order for `a`: plain, acute, grave, breve, circumflex,
    /// caron, ring, diaeresis, tilde, ogonek, macron, double-grave.
    #[test]
    fn locale_compare_orders_accents_like_icu() {
        let ordered = ["a", "á", "à", "ă", "â", "ǎ", "å", "ä", "ã", "ą", "ā", "ȁ"];
        let mut sorted = ordered.to_vec();
        sorted.sort_by(|left, right| locale_compare(left, right));
        assert_eq!(sorted, ordered);
        // Case is a lower level than the accent.
        assert_eq!(locale_compare("e", "É"), std::cmp::Ordering::Less);
    }

    /// ICU expands `ß` to `ss` and `æ` to `ae`, and the expanded form sorts first.
    #[test]
    fn locale_compare_handles_ligature_expansions() {
        assert_eq!(locale_compare("ss", "ß"), std::cmp::Ordering::Less);
        assert_eq!(locale_compare("ss", "SS"), std::cmp::Ordering::Less);
        assert_eq!(locale_compare("ae", "æ"), std::cmp::Ordering::Less);
        assert_eq!(locale_compare("æ", "Æ"), std::cmp::Ordering::Less);
    }

    /// The whole reason for the change: the dropdown order must match TS.
    #[test]
    fn file_suggestion_ties_follow_locale_order() {
        let mut labels = vec!["README.md", "apple.txt", "B.txt", "a.txt"];
        labels.sort_by(|left, right| locale_compare(left, right));
        assert_eq!(labels, vec!["a.txt", "apple.txt", "B.txt", "README.md"]);
    }

    // --- TUIR-9 regressions: Node `path` semantics (autocomplete.ts:4) --------

    /// On Windows `dirname`/`basename` must split on `\` as well, like
    /// `path.win32.dirname` / `path.win32.basename` (autocomplete.ts:587-588).
    #[test]
    fn path_helpers_follow_the_platform_flavour() {
        if cfg!(windows) {
            assert_eq!(dirname("src\\fo"), "src");
            assert_eq!(basename("src\\fo"), "fo");
            assert_eq!(dirname("src\\a\\b"), "src\\a");
            assert_eq!(basename("src\\a\\b"), "b");
            assert_eq!(dirname("\\"), "\\");
            assert_eq!(basename("\\"), "");
            assert_eq!(dirname("C:\\Users\\x"), "C:\\Users");
            assert_eq!(basename("C:\\Users\\x"), "x");
            // `path.win32.join` composes with `\`, so relative completions keep
            // the backslash form the user typed.
            assert_eq!(join_path("src", "name"), "src\\name");
            assert_eq!(join_path(".", "name"), "name");
            assert_eq!(join_path("C:\\proj", "src"), "C:\\proj\\src");
        } else {
            assert_eq!(dirname("src/fo"), "src");
            assert_eq!(basename("src/fo"), "fo");
            assert_eq!(dirname("/"), "/");
            assert_eq!(basename("/"), "");
            assert_eq!(join_path("src", "name"), "src/name");
            assert_eq!(join_path(".", "name"), "name");
        }
    }

    /// A backslash prefix must resolve to a real directory instead of "." so
    /// `src\fo` + Tab can list `src`. This is the user-visible TUIR-9 failure on
    /// Windows and the exact scenario in the audit.
    #[test]
    fn backslash_prefix_resolves_a_search_directory() {
        if !cfg!(windows) {
            return;
        }
        let expanded_prefix = "src\\fo";
        let search_dir = join_path(".", &dirname(expanded_prefix));
        assert_eq!(search_dir, "src");
        assert_eq!(basename(expanded_prefix), "fo");
    }

    /// A leading-root prefix must not be split into a partial drive fragment.
    #[test]
    fn root_and_drive_prefixes_are_not_split_midway() {
        if !cfg!(windows) {
            return;
        }
        assert_eq!(dirname("\\"), "\\");
        assert_eq!(dirname("/"), "/");
        assert_eq!(dirname("C:\\"), "C:\\");
        assert_eq!(basename("C:\\"), "");
        assert_eq!(win32_normalize("\\\\server\\share"), "\\\\server\\share\\");
    }

    /// `path.join` must not leave a doubled separator for an empty side.
    #[test]
    fn join_path_handles_empty_parts_like_node() {
        assert_eq!(join_path("", "name"), "name");
        assert_eq!(join_path("src", ""), "src");
        assert_eq!(join_path("", ""), ".");
    }

    #[test]
    fn fd_query_escapes_regex_characters() {
        assert_eq!(build_fd_path_query("src/foo"), "src[\\\\/]foo");
        assert_eq!(build_fd_path_query("src/"), "src[\\\\/]");
        assert_eq!(build_fd_path_query("foo"), "foo");
        assert_eq!(build_fd_path_query("a.b/c+d"), "a\\.b[\\\\/]c\\+d");
    }

    #[test]
    fn path_prefix_parsing_matches_typescript() {
        assert_eq!(
            parse_path_prefix("@\"src"),
            ParsedPathPrefix {
                raw_prefix: "src".to_string(),
                is_at_prefix: true,
                is_quoted_prefix: true
            }
        );
        assert_eq!(
            parse_path_prefix("src"),
            ParsedPathPrefix {
                raw_prefix: "src".to_string(),
                is_at_prefix: false,
                is_quoted_prefix: false
            }
        );
    }

    #[test]
    fn completion_values_quote_when_needed() {
        assert_eq!(
            build_completion_value(
                "src/a b",
                CompletionValueOptions {
                    is_directory: false,
                    is_at_prefix: true,
                    is_quoted_prefix: false
                }
            ),
            "@\"src/a b\""
        );
        assert_eq!(
            build_completion_value(
                "src/a",
                CompletionValueOptions {
                    is_directory: false,
                    is_at_prefix: false,
                    is_quoted_prefix: false
                }
            ),
            "src/a"
        );
    }

    #[test]
    fn at_prefix_extraction() {
        let provider = CombinedAutocompleteProvider::new(Vec::new(), ".", None);
        assert_eq!(provider.extract_at_prefix("hi @src/a"), Some("@src/a".to_string()));
        assert_eq!(provider.extract_at_prefix("hi src/a"), None);
        assert_eq!(provider.extract_at_prefix("hi @\"src/a"), Some("@\"src/a".to_string()));
    }

    #[test]
    fn slash_command_completion_rewrites_line() {
        let provider = CombinedAutocompleteProvider::new(
            vec![CommandEntry::Slash(SlashCommand {
                name: "help".to_string(),
                aliases: vec!["h".to_string()],
                description: Some("Show help".to_string()),
                argument_hint: None,
                source_tag: None,
                takes_argument: Some(false),
                get_argument_completions: None,
            })],
            ".",
            None,
        );
        let item = AutocompleteItem {
            value: "help".to_string(),
            label: "help".to_string(),
            description: None,
            argument_hint: None,
            source_tag: None,
            takes_argument: None,
        };
        let result = provider.apply_completion(&lines(&["/he"]), 0, 3, &item, "/he");
        assert_eq!(result.lines, lines(&["/help "]));
        assert_eq!(result.cursor_col, 6);
    }

    #[test]
    fn directory_completion_keeps_quote_cursor() {
        let provider = CombinedAutocompleteProvider::new(Vec::new(), ".", None);
        let item = AutocompleteItem {
            value: "@\"src/\"".to_string(),
            label: "src/".to_string(),
            description: None,
            argument_hint: None,
            source_tag: None,
            takes_argument: None,
        };
        let result = provider.apply_completion(&lines(&["@\"sr"]), 0, 4, &item, "@\"sr");
        // Directory completions get no trailing space so the user can keep typing.
        assert_eq!(result.lines, lines(&["@\"src/\""]));
        assert_eq!(result.cursor_col, 6);
    }

    #[test]
    fn should_trigger_file_completion_skips_command_names() {
        let provider = CombinedAutocompleteProvider::new(Vec::new(), ".", None);
        assert!(!provider.should_trigger_file_completion(&lines(&["/he"]), 0, 3));
        assert!(provider.should_trigger_file_completion(&lines(&["hello"]), 0, 5));
    }
}
