//! Port of packages/coding-agent/src/core/prompt-templates.ts

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::slash_commands::parse_slash_command;
use crate::core::source_info::{create_synthetic_source_info, SourceInfo, SyntheticSourceInfoOptions};
use crate::utils::frontmatter::parse_frontmatter;

/// `CONFIG_DIR_NAME` from `config.ts` (package.json `piConfig.configDir`).
pub const CONFIG_DIR_NAME: &str = ".prime/agent";

/// Represents a prompt template loaded from a markdown file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    pub content: String,
    pub source_info: SourceInfo,
    /// Absolute path to the template file.
    pub file_path: String,
}

/// Parse command arguments respecting quoted strings (bash-style).
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;
    // An explicitly quoted token is an argument even when empty ("" or '').
    let mut quoted = false;

    for ch in args_string.chars() {
        if let Some(quote) = in_quote {
            if ch == quote {
                in_quote = None;
            } else {
                current.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = Some(ch);
            quoted = true;
        } else if ch == '\t' || is_unicode_space_separator(ch) {
            if !current.is_empty() || quoted {
                args.push(std::mem::take(&mut current));
                quoted = false;
            }
        } else {
            current.push(ch);
        }
    }

    if !current.is_empty() || quoted {
        args.push(current);
    }

    args
}

/// `[\t\p{Zs}]` - tab plus the Unicode space separators (not `\n`/`\r`).
fn is_unicode_space_separator(ch: char) -> bool {
    matches!(
        ch,
        '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
    )
}

/// Substitute argument placeholders in template content.
///
/// Supports `$1`, `$2`, ... for positional args, `$@` and `$ARGUMENTS` for all
/// args, `${@:N}` for args from Nth onwards, and `${@:N:L}` for L args starting
/// at Nth. Replacement happens on the template string only; argument values
/// containing `$1`, `$@` or `$ARGUMENTS` are NOT recursively substituted.
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let all_args = args.join(" ");
    let chars: Vec<char> = content.chars().collect();
    let mut out = String::with_capacity(content.len());
    let mut index = 0;

    while index < chars.len() {
        if chars[index] != '$' {
            out.push(chars[index]);
            index += 1;
            continue;
        }
        let Some((replacement, consumed)) = match_placeholder(&chars[index..], args, &all_args) else {
            out.push(chars[index]);
            index += 1;
            continue;
        };
        out.push_str(&replacement);
        index += consumed;
    }

    out
}

/// Match `\$(?:(\d+)|\{@:(\d+)(?::(\d+))?\}|ARGUMENTS|@)` at the start of `rest`.
/// Returns the replacement text and how many chars were consumed.
fn match_placeholder(rest: &[char], args: &[String], all_args: &str) -> Option<(String, usize)> {
    debug_assert_eq!(rest[0], '$');
    let tail = &rest[1..];
    if tail.is_empty() {
        return None;
    }

    // `$(?:(\d+)...)`: positional digits.
    if tail[0].is_ascii_digit() {
        let digits: String = tail.iter().take_while(|ch| ch.is_ascii_digit()).collect();
        let position: usize = digits.parse().ok()?;
        let value = if position >= 1 {
            args.get(position - 1).cloned().unwrap_or_default()
        } else {
            String::new()
        };
        return Some((value, 1 + digits.len()));
    }

    // `\$\{@:(\d+)(?::(\d+))?\}`
    if tail[0] == '{' {
        let mut cursor = 1;
        if tail.get(cursor) != Some(&'@') || tail.get(cursor + 1) != Some(&':') {
            return None;
        }
        cursor += 2;
        let start_digits: String = tail[cursor..]
            .iter()
            .take_while(|ch| ch.is_ascii_digit())
            .collect();
        if start_digits.is_empty() {
            return None;
        }
        cursor += start_digits.len();
        let mut length_digits: Option<String> = None;
        if tail.get(cursor) == Some(&':') {
            cursor += 1;
            let digits: String = tail[cursor..]
                .iter()
                .take_while(|ch| ch.is_ascii_digit())
                .collect();
            if digits.is_empty() {
                return None;
            }
            cursor += digits.len();
            length_digits = Some(digits);
        }
        if tail.get(cursor) != Some(&'}') {
            return None;
        }
        cursor += 1;

        let start_value: i64 = start_digits.parse().ok()?;
        // Slices are 1-indexed; zero also starts at the first argument.
        let start = (start_value - 1).max(0) as usize;
        let end = match &length_digits {
            Some(digits) => {
                let length: i64 = digits.parse().ok()?;
                Some(((start as i64) + length).max(0) as usize)
            }
            None => None,
        };
        let slice_end = end.unwrap_or(args.len()).min(args.len());
        let slice = if start >= args.len() {
            String::new()
        } else {
            args[start..slice_end.max(start)].join(" ")
        };
        return Some((slice, 1 + cursor));
    }

    // `ARGUMENTS`
    let arguments: Vec<char> = "ARGUMENTS".chars().collect();
    if tail.len() >= arguments.len() && tail[..arguments.len()] == arguments[..] {
        return Some((all_args.to_string(), 1 + arguments.len()));
    }

    // `@`
    if tail[0] == '@' {
        return Some((all_args.to_string(), 2));
    }

    None
}

fn load_template_from_file(file_path: &str, source_info: SourceInfo) -> Option<PromptTemplate> {
    let raw_content = std::fs::read_to_string(file_path).ok()?;
    let parsed = parse_frontmatter(&raw_content).ok()?;
    let frontmatter = parsed.frontmatter;
    let body = parsed.body;

    let name = basename(file_path).trim_end_matches(".md").to_string();

    // Get description from frontmatter or first non-empty line.
    let mut description = frontmatter
        .get("description")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if description.is_empty() {
        if let Some(first_line) = body.split('\n').find(|line| !line.trim().is_empty()) {
            // Truncate if too long. JS `slice(0, 60)` counts UTF-16 units; the
            // ASCII-only prefix rule keeps the same result for the usual case.
            description = js_slice(first_line, 0, 60);
            if first_line.chars().count() > 60 {
                description.push_str("...");
            }
        }
    }
    let argument_hint = frontmatter
        .get("argument-hint")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    Some(PromptTemplate {
        name,
        description,
        argument_hint,
        content: body,
        source_info,
        file_path: file_path.to_string(),
    })
}

/// JS `String.prototype.slice(0, end)` for the ASCII prefix case used here.
fn js_slice(value: &str, start: usize, end: usize) -> String {
    value.chars().skip(start).take(end.saturating_sub(start)).collect()
}

fn basename(value: &str) -> String {
    let trimmed = value.trim_end_matches(['/', '\\']);
    match trimmed.rsplit(['/', '\\']).next() {
        Some(part) if !part.is_empty() => part.to_string(),
        _ => String::new(),
    }
}

/// Scan a directory for `.md` files (non-recursive) and load them as prompt templates.
fn load_templates_from_dir(
    dir: &str,
    get_source_info: &dyn Fn(&str) -> SourceInfo,
) -> Vec<PromptTemplate> {
    let mut templates: Vec<PromptTemplate> = Vec::new();

    if !Path::new(dir).exists() {
        return templates;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return templates;
    };

    // `readdirSync` order is filesystem order; keep the same, sorting is not
    // observable in the TypeScript so do not introduce it here.
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let full_path = join_path(dir, &name);

        // For symlinks, check if they point to a file.
        let mut is_file = entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false);
        if entry
            .file_type()
            .map(|kind| kind.is_symlink())
            .unwrap_or(false)
        {
            match std::fs::metadata(&full_path) {
                Ok(metadata) => is_file = metadata.is_file(),
                // Broken symlink, skip it.
                Err(_) => continue,
            }
        }

        if is_file && name.ends_with(".md") {
            if let Some(template) = load_template_from_file(&full_path, get_source_info(&full_path)) {
                templates.push(template);
            }
        }
    }

    templates
}

/// `LoadPromptTemplatesOptions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadPromptTemplatesOptions {
    /// Working directory for project-local templates.
    pub cwd: String,
    /// Agent config directory for global templates.
    pub agent_dir: String,
    /// Explicit prompt template paths (files or directories).
    pub prompt_paths: Vec<String>,
    /// Include default prompt directories.
    pub include_defaults: bool,
}

fn normalize_path(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed == "~" {
        return home_dir();
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return join_path(&home_dir(), rest);
    }
    if let Some(rest) = trimmed.strip_prefix('~') {
        return join_path(&home_dir(), rest);
    }
    trimmed.to_string()
}

fn home_dir() -> String {
    dirs::home_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn resolve_prompt_path(p: &str, cwd: &str) -> String {
    let normalized = normalize_path(p);
    if Path::new(&normalized).is_absolute() {
        normalized
    } else {
        resolve_path(cwd, &normalized)
    }
}

fn resolve_path(base: &str, part: &str) -> String {
    crate::utils::paths::resolve_path(&join_path(base, part))
}

fn join_path(base: &str, part: &str) -> String {
    let base = base.trim_end_matches(['/', '\\']);
    format!("{base}{}{part}", std::path::MAIN_SEPARATOR)
}

fn is_under_path(target: &str, root: &str) -> bool {
    let normalized_root = crate::utils::paths::resolve_path(root);
    if target == normalized_root {
        return true;
    }
    let sep = std::path::MAIN_SEPARATOR;
    let prefix = if normalized_root.ends_with(sep) {
        normalized_root
    } else {
        format!("{normalized_root}{sep}")
    };
    target.starts_with(&prefix)
}

/// Load all prompt templates from:
/// 1. Global: `agentDir/prompts/`
/// 2. Project: `cwd/{CONFIG_DIR_NAME}/prompts/`
/// 3. Explicit prompt paths
pub fn load_prompt_templates(options: &LoadPromptTemplatesOptions) -> Vec<PromptTemplate> {
    let resolved_cwd = options.cwd.clone();
    let resolved_agent_dir = options.agent_dir.clone();
    let prompt_paths = &options.prompt_paths;
    let include_defaults = options.include_defaults;

    let mut templates: Vec<PromptTemplate> = Vec::new();

    let global_prompts_dir = if options.agent_dir.is_empty() {
        resolved_agent_dir.clone()
    } else {
        join_path(&options.agent_dir, "prompts")
    };
    let project_prompts_dir = resolve_path(&resolved_cwd, CONFIG_DIR_NAME);
    let project_prompts_dir = join_path(&project_prompts_dir, "prompts");

    let get_source_info = |resolved_path: &str| -> SourceInfo {
        if is_under_path(resolved_path, &global_prompts_dir) {
            return create_synthetic_source_info(
                resolved_path,
                &SyntheticSourceInfoOptions {
                    source: "local".to_string(),
                    scope: Some("user".to_string()),
                    origin: None,
                    base_dir: Some(global_prompts_dir.clone()),
                },
            );
        }
        if is_under_path(resolved_path, &project_prompts_dir) {
            return create_synthetic_source_info(
                resolved_path,
                &SyntheticSourceInfoOptions {
                    source: "local".to_string(),
                    scope: Some("project".to_string()),
                    origin: None,
                    base_dir: Some(project_prompts_dir.clone()),
                },
            );
        }
        let base_dir = match std::fs::metadata(resolved_path) {
            Ok(metadata) if metadata.is_dir() => resolved_path.to_string(),
            _ => parent_dir(resolved_path),
        };
        create_synthetic_source_info(
            resolved_path,
            &SyntheticSourceInfoOptions {
                source: "local".to_string(),
                scope: None,
                origin: None,
                base_dir: Some(base_dir),
            },
        )
    };

    if include_defaults {
        templates.extend(load_templates_from_dir(&global_prompts_dir, &get_source_info));
        templates.extend(load_templates_from_dir(&project_prompts_dir, &get_source_info));
    }

    // 3. Load explicit prompt paths.
    for raw_path in prompt_paths {
        let resolved_path = resolve_prompt_path(raw_path, &resolved_cwd);
        if !Path::new(&resolved_path).exists() {
            continue;
        }

        let Ok(metadata) = std::fs::metadata(&resolved_path) else {
            continue;
        };
        if metadata.is_dir() {
            templates.extend(load_templates_from_dir(&resolved_path, &get_source_info));
        } else if metadata.is_file() && resolved_path.ends_with(".md") {
            if let Some(template) =
                load_template_from_file(&resolved_path, get_source_info(&resolved_path))
            {
                templates.push(template);
            }
        }
    }

    templates
}

fn parent_dir(value: &str) -> String {
    let path = PathBuf::from(value);
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().to_string(),
        _ => value.to_string(),
    }
}

/// Expand a prompt template if it matches a template name.
/// Returns the expanded content or the original text if not a template.
pub fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String {
    if !text.starts_with('/') {
        return text.to_string();
    }

    let Some(parsed) = parse_slash_command(text) else {
        return text.to_string();
    };
    let template_name = parsed.name;
    let args_string = parsed.args;

    match templates.iter().find(|template| template.name == template_name) {
        Some(template) => {
            let args = parse_command_args(&args_string);
            substitute_args(&template.content, &args)
        }
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(name: &str, content: &str) -> PromptTemplate {
        PromptTemplate {
            name: name.to_string(),
            description: String::new(),
            argument_hint: None,
            content: content.to_string(),
            source_info: SourceInfo {
                path: format!("/t/{name}.md"),
                source: "local".to_string(),
                scope: "temporary".to_string(),
                origin: "top-level".to_string(),
                base_dir: None,
            },
            file_path: format!("/t/{name}.md"),
        }
    }

    #[test]
    fn parse_command_args_respects_quotes() {
        assert_eq!(parse_command_args("a b c"), ["a", "b", "c"]);
        assert_eq!(parse_command_args("  a   b  "), ["a", "b"]);
        assert_eq!(parse_command_args(r#"one "two three" four"#), ["one", "two three", "four"]);
        assert_eq!(parse_command_args("one 'two three'"), ["one", "two three"]);
        // An explicitly quoted empty token is still an argument.
        assert_eq!(parse_command_args(r#""""#), [""]);
        assert_eq!(parse_command_args("''"), [""]);
        assert_eq!(parse_command_args(""), Vec::<String>::new());
        assert_eq!(parse_command_args("   "), Vec::<String>::new());
        // Tabs and Unicode spaces split; newlines do not.
        assert_eq!(parse_command_args("a\tb\u{00a0}c"), ["a", "b", "c"]);
        assert_eq!(parse_command_args("a\nb"), ["a\nb"]);
    }

    #[test]
    fn substitute_args_handles_positional_and_all_arguments() {
        let args = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        assert_eq!(substitute_args("$1 and $2", &args), "one and two");
        assert_eq!(substitute_args("$@", &args), "one two three");
        assert_eq!(substitute_args("$ARGUMENTS", &args), "one two three");
        assert_eq!(substitute_args("missing:$4", &args), "missing:");
        assert_eq!(substitute_args("${@:2}", &args), "two three");
        assert_eq!(substitute_args("${@:2:1}", &args), "two");
        assert_eq!(substitute_args("${@:0}", &args), "one two three");
        assert_eq!(substitute_args("no placeholders", &args), "no placeholders");
        assert_eq!(substitute_args("$", &args), "$");
        assert_eq!(substitute_args("$ARGUMENT", &args), "$ARGUMENT");
    }

    #[test]
    fn substitute_args_does_not_recurse_into_argument_values() {
        let args = vec!["$1".to_string()];
        assert_eq!(substitute_args("value=$1", &args), "value=$1");
    }

    #[test]
    fn expand_prompt_template_matches_names_and_passes_args() {
        let templates = vec![template("greet", "Hello $1! $@")];
        assert_eq!(
            expand_prompt_template("/greet world extra", &templates),
            "Hello world! world extra"
        );
        // Unknown template returns the original text.
        assert_eq!(expand_prompt_template("/nope x", &templates), "/nope x");
        assert_eq!(expand_prompt_template("plain text", &templates), "plain text");
    }

    #[test]
    fn load_prompt_templates_reads_files_and_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = dir.path().join("prompts");
        std::fs::create_dir_all(&prompts).unwrap();
        std::fs::write(
            prompts.join("first.md"),
            "---\ndescription: From frontmatter\nargument-hint: \"[x]\"\n---\nBody $1",
        )
        .unwrap();
        std::fs::write(prompts.join("second.md"), "\nFirst line description that is longer than sixty characters in total yes\n").unwrap();
        std::fs::write(prompts.join("ignored.txt"), "not markdown").unwrap();

        let options = LoadPromptTemplatesOptions {
            cwd: dir.path().to_string_lossy().to_string(),
            agent_dir: dir.path().to_string_lossy().to_string(),
            prompt_paths: vec![prompts.to_string_lossy().to_string()],
            include_defaults: false,
        };
        let mut loaded = load_prompt_templates(&options);
        loaded.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "first");
        assert_eq!(loaded[0].description, "From frontmatter");
        assert_eq!(loaded[0].argument_hint.as_deref(), Some("[x]"));
        assert_eq!(loaded[0].content, "Body $1");
        assert_eq!(loaded[0].source_info.scope, "user");

        assert_eq!(loaded[1].name, "second");
        assert_eq!(loaded[1].description.chars().count(), 63);
        assert!(loaded[1].description.ends_with("..."));
    }

    #[test]
    fn missing_paths_are_skipped() {
        let options = LoadPromptTemplatesOptions {
            cwd: "/definitely/missing".to_string(),
            agent_dir: "/definitely/missing".to_string(),
            prompt_paths: vec!["/definitely/missing/prompts".to_string()],
            include_defaults: false,
        };
        assert!(load_prompt_templates(&options).is_empty());
    }
}
