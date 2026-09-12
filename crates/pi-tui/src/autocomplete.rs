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

/// Port of `walkDirectoryWithFd`. The child process is spawned and awaited on the
/// current thread; `signal` mirrors the TypeScript `AbortSignal`.
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

    let child = match std::process::Command::new(fd_path)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return Vec::new(),
    };

    let output = child.wait_with_output();
    if signal.is_aborted() {
        return Vec::new();
    }
    let output = match output {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };
    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
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
            a.label.cmp(&b.label)
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

/// Port of `path.join` (display separators are normalized by `to_display_path`).
fn join_path(base: &str, part: &str) -> String {
    if base.is_empty() {
        return part.to_string();
    }
    if part.is_empty() {
        return base.to_string();
    }
    if part.starts_with('/') && !base.ends_with('/') {
        return format!("{base}{part}");
    }
    let base = base.trim_end_matches('/');
    let part = part.trim_start_matches('/');
    format!("{base}/{part}")
}

/// Port of `path.dirname`.
fn dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        None => {
            if trimmed.is_empty() {
                ".".to_string()
            } else {
                ".".to_string()
            }
        }
        Some(0) => "/".to_string(),
        Some(index) => trimmed[..index].to_string(),
    }
}

/// Port of `path.basename`.
fn basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        None => trimmed.to_string(),
        Some(index) => trimmed[index + 1..].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
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
