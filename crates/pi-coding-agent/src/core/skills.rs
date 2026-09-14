//! Port of packages/coding-agent/src/core/skills.ts
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::diagnostics::ResourceDiagnostic;
use crate::core::source_info::{
    create_synthetic_source_info, SourceInfo, SyntheticSourceInfoOptions, SOURCE_SCOPE_PROJECT,
    SOURCE_SCOPE_USER,
};
use crate::utils::frontmatter::parse_frontmatter;
use crate::utils::paths::canonicalize_path;

/// `getLogger("coding-agent.skills")` - warnings only; logging is a no-op here
/// because the shared logger lives in pi-ai (blocked_on: pi-ai logger binding).
fn log_warn(message: &str, dir: &str, error: Option<&str>) {
    let _ = (message, dir, error);
}

/// Max name length per spec
const MAX_NAME_LENGTH: usize = 64;

/// Max description length per spec
const MAX_DESCRIPTION_LENGTH: usize = 1024;

const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];

/// Port of the `ignore` npm package: ordered patterns where the last match wins.
#[derive(Debug, Clone, Default)]
pub struct IgnoreMatcher {
    rules: Vec<IgnoreRule>,
}

#[derive(Debug, Clone)]
struct IgnoreRule {
    negated: bool,
    pattern: String,
    directory_only: bool,
    regex: regex::Regex,
}

impl IgnoreMatcher {
    pub fn new() -> IgnoreMatcher {
        IgnoreMatcher { rules: Vec::new() }
    }

    pub fn add(&mut self, patterns: &[String]) {
        for pattern in patterns {
            if let Some(rule) = compile_rule(pattern) {
                self.rules.push(rule);
            }
        }
    }

    /// Last matching pattern wins, as in the `ignore` package.
    pub fn ignores(&self, path: &str) -> bool {
        let mut ignored = false;
        for rule in &self.rules {
            let matched = if rule.directory_only {
                path.ends_with('/') && rule.regex.is_match(path)
            } else {
                rule.regex.is_match(path)
            };
            if matched {
                ignored = !rule.negated;
            }
        }
        ignored
    }
}

fn compile_rule(pattern: &str) -> Option<IgnoreRule> {
    let negated = pattern.starts_with('!');
    let body = if negated { &pattern[1..] } else { pattern };
    let directory_only = body.ends_with('/');
    let body = body.trim_end_matches('/');
    // A pattern that contains a slash (other than a trailing one) is anchored
    // to the search root, as in gitignore.
    let anchored = body.starts_with('/') || body.trim_end_matches('/').contains('/');
    let body = body.trim_start_matches('/');
    let mut expression = String::from("^");
    if !anchored {
        expression.push_str("(?:.*/)?");
    }
    let mut chars = body.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    expression.push_str(".*");
                } else {
                    expression.push_str("[^/]*");
                }
            }
            '?' => expression.push_str("[^/]"),
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '\\' => {
                expression.push('\\');
                expression.push(character);
            }
            '[' => {
                expression.push('[');
                for next in chars.by_ref() {
                    expression.push(next);
                    if next == ']' {
                        break;
                    }
                }
            }
            other => expression.push(other),
        }
    }
    expression.push_str(if directory_only { "(?:/.*)?$" } else { "$" });
    let regex = regex::Regex::new(&expression).ok()?;
    Some(IgnoreRule {
        negated,
        pattern: body.to_string(),
        directory_only,
        regex,
    })
}

fn to_posix_path(value: &str) -> String {
    value.replace('\\', "/")
}

fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("\\#") {
        return None;
    }
    let mut pattern = line.to_string();
    let mut negated = false;
    if pattern.starts_with('!') {
        negated = true;
        pattern = pattern[1..].to_string();
    } else if pattern.starts_with("\\!") {
        pattern = pattern[1..].to_string();
    }
    if pattern.starts_with('/') {
        pattern = pattern[1..].to_string();
    }
    let prefixed = if prefix.is_empty() {
        pattern
    } else {
        format!("{prefix}{pattern}")
    };
    Some(if negated {
        format!("!{prefixed}")
    } else {
        prefixed
    })
}

fn add_ignore_rules(matcher: &mut IgnoreMatcher, dir: &Path, root_dir: &Path) {
    let relative_dir = relative_path(root_dir, dir);
    let prefix = if relative_dir.is_empty() {
        String::new()
    } else {
        format!("{}/", to_posix_path(&relative_dir))
    };
    for filename in IGNORE_FILE_NAMES {
        let ignore_path = dir.join(filename);
        if !ignore_path.exists() {
            continue;
        }
        // Unreadable ignore file: skip it rather than failing skill discovery.
        let content = match std::fs::read_to_string(&ignore_path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let patterns: Vec<String> = content
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .filter_map(|line| prefix_ignore_pattern(line, &prefix))
            .collect();
        if !patterns.is_empty() {
            matcher.add(&patterns);
        }
    }
}

fn relative_path(root: &Path, target: &Path) -> String {
    let root_components: Vec<_> = root.components().collect();
    let target_components: Vec<_> = target.components().collect();
    let mut common = 0;
    while common < root_components.len().min(target_components.len())
        && root_components[common] == target_components[common]
    {
        common += 1;
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..root_components.len() {
        parts.push("..".to_string());
    }
    for component in &target_components[common..] {
        parts.push(component.as_os_str().to_string_lossy().to_string());
    }
    parts.join("/")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillFrontmatter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, rename = "disable-model-invocation")]
    pub disable_model_invocation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillKind {
    Markdown,
    Python,
}

impl SkillKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SkillKind::Markdown => "markdown",
            SkillKind::Python => "python",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillPythonMetadata {
    #[serde(rename = "importName")]
    pub import_name: String,
    #[serde(rename = "packagePath")]
    pub package_path: String,
    #[serde(rename = "pyprojectPath")]
    pub pyproject_path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaseSkill {
    pub name: String,
    pub description: String,
    #[serde(rename = "filePath")]
    pub file_path: String,
    #[serde(rename = "baseDir")]
    pub base_dir: String,
    #[serde(rename = "sourceInfo")]
    pub source_info: SourceInfo,
    #[serde(rename = "disableModelInvocation")]
    pub disable_model_invocation: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkdownSkill {
    #[serde(flatten)]
    pub base: BaseSkill,
    pub kind: SkillKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PythonSkill {
    #[serde(flatten)]
    pub base: BaseSkill,
    pub kind: SkillKind,
    pub python: SkillPythonMetadata,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Skill {
    Markdown(MarkdownSkill),
    Python(PythonSkill),
}

// The TypeScript type is a discriminated union on `kind`; serde's untagged enum
// cannot combine the flattened `base` fields with the variant payload, so the
// discriminator is read explicitly.
impl Serialize for Skill {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Skill::Markdown(skill) => skill.serialize(serializer),
            Skill::Python(skill) => skill.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Skill {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Skill, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if value.get("kind").and_then(Value::as_str) == Some("python") {
            serde_json::from_value(value)
                .map(Skill::Python)
                .map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value(value)
                .map(Skill::Markdown)
                .map_err(serde::de::Error::custom)
        }
    }
}

impl Skill {
    pub fn name(&self) -> &str {
        match self {
            Skill::Markdown(skill) => &skill.base.name,
            Skill::Python(skill) => &skill.base.name,
        }
    }

    pub fn file_path(&self) -> &str {
        match self {
            Skill::Markdown(skill) => &skill.base.file_path,
            Skill::Python(skill) => &skill.base.file_path,
        }
    }

    pub fn kind(&self) -> SkillKind {
        match self {
            Skill::Markdown(_) => SkillKind::Markdown,
            Skill::Python(_) => SkillKind::Python,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PythonSkillRuntimeInfo {
    #[serde(flatten)]
    pub python: SkillPythonMetadata,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadSkillsResult {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// Validate skill name per Agent Skills spec.
/// Returns array of validation error messages (empty if valid).
fn validate_name(name: &str, parent_dir_name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if name != parent_dir_name {
        errors.push(format!(
            "name \"{name}\" does not match parent directory \"{parent_dir_name}\""
        ));
    }
    if name.chars().count() > MAX_NAME_LENGTH {
        errors.push(format!(
            "name exceeds {MAX_NAME_LENGTH} characters ({})",
            name.chars().count()
        ));
    }
    if !regex::Regex::new(r"^[a-z0-9-]+$").unwrap().is_match(name) {
        errors.push(
            "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)"
                .to_string(),
        );
    }
    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".to_string());
    }
    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".to_string());
    }
    errors
}

/// Validate description per Agent Skills spec.
fn validate_description(description: Option<&str>) -> Vec<String> {
    let mut errors = Vec::new();
    match description {
        None => errors.push("description is required".to_string()),
        Some(value) if value.trim().is_empty() => {
            errors.push("description is required".to_string())
        }
        Some(value) if value.chars().count() > MAX_DESCRIPTION_LENGTH => errors.push(format!(
            "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({})",
            value.chars().count()
        )),
        _ => {}
    }
    errors
}

#[derive(Debug, Clone)]
pub struct LoadSkillsFromDirOptions {
    /// Directory to scan for skills
    pub dir: String,
    /// Source identifier for these skills
    pub source: String,
}

fn create_skill_source_info(file_path: &str, base_dir: &str, source: &str) -> SourceInfo {
    // `origin` is left to the synthetic default, as in the TypeScript.
    match source {
        "user" => create_synthetic_source_info(
            file_path,
            &SyntheticSourceInfoOptions {
                source: "local".to_string(),
                scope: Some(SOURCE_SCOPE_USER.to_string()),
                base_dir: Some(base_dir.to_string()),
                ..Default::default()
            },
        ),
        "project" => create_synthetic_source_info(
            file_path,
            &SyntheticSourceInfoOptions {
                source: "local".to_string(),
                scope: Some(SOURCE_SCOPE_PROJECT.to_string()),
                base_dir: Some(base_dir.to_string()),
                ..Default::default()
            },
        ),
        "path" => create_synthetic_source_info(
            file_path,
            &SyntheticSourceInfoOptions {
                source: "local".to_string(),
                base_dir: Some(base_dir.to_string()),
                ..Default::default()
            },
        ),
        other => create_synthetic_source_info(
            file_path,
            &SyntheticSourceInfoOptions {
                source: other.to_string(),
                base_dir: Some(base_dir.to_string()),
                ..Default::default()
            },
        ),
    }
}

fn python_import_name_for_skill(name: &str) -> String {
    name.replace('-', "_")
}

fn is_valid_python_import_name(name: &str) -> bool {
    regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$")
        .unwrap()
        .is_match(name)
}

fn warning(message: String, path: &str) -> ResourceDiagnostic {
    ResourceDiagnostic {
        diagnostic_type: crate::core::diagnostics::RESOURCE_DIAGNOSTIC_WARNING.to_string(),
        message,
        path: Some(path.to_string()),
        collision: None,
    }
}

fn detect_python_skill(
    skill_dir: &Path,
    name: &str,
    diagnostics: &mut Vec<ResourceDiagnostic>,
) -> Option<SkillPythonMetadata> {
    let pyproject_path = skill_dir.join("pyproject.toml");
    if !pyproject_path.exists() {
        return None;
    }
    match std::fs::metadata(&pyproject_path) {
        Ok(metadata) if metadata.is_file() => {}
        _ => return None,
    }
    let import_name = python_import_name_for_skill(name);
    if !is_valid_python_import_name(&import_name) {
        diagnostics.push(warning(
            format!("python skill import name \"{import_name}\" is invalid"),
            &pyproject_path.to_string_lossy(),
        ));
        return None;
    }
    let package_init_path = skill_dir.join("src").join(&import_name).join("__init__.py");
    match std::fs::metadata(&package_init_path) {
        Ok(metadata) if metadata.is_file() => {}
        _ => {
            diagnostics.push(warning(
                format!("python skill package src/{import_name}/__init__.py not found"),
                &pyproject_path.to_string_lossy(),
            ));
            return None;
        }
    }
    Some(SkillPythonMetadata {
        import_name,
        package_path: skill_dir.to_string_lossy().to_string(),
        pyproject_path: pyproject_path.to_string_lossy().to_string(),
    })
}

pub fn get_python_skill_runtime_info(skills: &[Skill]) -> Vec<PythonSkillRuntimeInfo> {
    skills
        .iter()
        .filter_map(|skill| match skill {
            Skill::Python(python) => Some(PythonSkillRuntimeInfo {
                name: python.base.name.clone(),
                python: python.python.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Load skills from a directory.
///
/// Discovery rules:
/// - if a directory contains SKILL.md, treat it as a skill root and do not recurse further
/// - otherwise, load direct .md children in the root
/// - recurse into subdirectories to find SKILL.md
pub fn load_skills_from_dir(options: LoadSkillsFromDirOptions) -> LoadSkillsResult {
    load_skills_from_dir_internal(&options.dir, &options.source, true, None, None)
}

fn load_skills_from_dir_internal(
    dir: &str,
    source: &str,
    include_root_files: bool,
    matcher: Option<&mut IgnoreMatcher>,
    root_dir: Option<&str>,
) -> LoadSkillsResult {
    let mut skills: Vec<Skill> = Vec::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    if !Path::new(dir).exists() {
        return LoadSkillsResult {
            skills,
            diagnostics,
        };
    }
    let root = root_dir.unwrap_or(dir).to_string();
    // One matcher is shared by the whole walk, exactly like the TypeScript
    // passes a single `ig` down the recursion.
    let mut owned_matcher;
    let matcher: &mut IgnoreMatcher = match matcher {
        Some(matcher) => matcher,
        None => {
            owned_matcher = IgnoreMatcher::new();
            &mut owned_matcher
        }
    };
    add_ignore_rules(matcher, Path::new(dir), Path::new(&root));
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            log_warn("skill directory scan failed", dir, Some(&error.to_string()));
            return LoadSkillsResult {
                skills,
                diagnostics,
            };
        }
    };
    let mut children: Vec<(String, bool, bool, bool)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let file_type = entry.file_type().ok();
        let is_symlink = file_type.map(|value| value.is_symlink()).unwrap_or(false);
        let full_path = Path::new(dir).join(&name);
        let mut is_file = file_type.map(|value| value.is_file()).unwrap_or(false);
        let mut is_directory = file_type.map(|value| value.is_dir()).unwrap_or(false);
        if is_symlink {
            match std::fs::metadata(&full_path) {
                Ok(metadata) => {
                    is_file = metadata.is_file();
                    is_directory = metadata.is_dir();
                }
                Err(_) => continue,
            }
        }
        children.push((name, is_file, is_directory, is_symlink));
    }

    for (name, is_file, _, _) in &children {
        if name != "SKILL.md" {
            continue;
        }
        let full_path = Path::new(dir).join(name);
        let rel_path = to_posix_path(&relative_path(Path::new(&root), &full_path));
        if !is_file || matcher.ignores(&rel_path) {
            continue;
        }
        let result = load_skill_from_file(&full_path.to_string_lossy(), source);
        if let Some(skill) = result.skill {
            skills.push(skill);
        }
        diagnostics.extend(result.diagnostics);
        return LoadSkillsResult {
            skills,
            diagnostics,
        };
    }

    for (name, is_file, is_directory, _) in &children {
        if name.starts_with('.') {
            continue;
        }
        if name == "node_modules" {
            continue;
        }
        let full_path = Path::new(dir).join(name);
        let rel_path = to_posix_path(&relative_path(Path::new(&root), &full_path));
        let ignore_path = if *is_directory {
            format!("{rel_path}/")
        } else {
            rel_path
        };
        if matcher.ignores(&ignore_path) {
            continue;
        }
        if *is_directory {
            let sub_result = load_skills_from_dir_internal(
                &full_path.to_string_lossy(),
                source,
                false,
                Some(&mut *matcher),
                Some(&root),
            );
            skills.extend(sub_result.skills);
            diagnostics.extend(sub_result.diagnostics);
            continue;
        }
        if !is_file || !include_root_files || !name.ends_with(".md") {
            continue;
        }
        let result = load_skill_from_file(&full_path.to_string_lossy(), source);
        if let Some(skill) = result.skill {
            skills.push(skill);
        }
        diagnostics.extend(result.diagnostics);
    }

    LoadSkillsResult {
        skills,
        diagnostics,
    }
}

struct LoadedSkill {
    skill: Option<Skill>,
    diagnostics: Vec<ResourceDiagnostic>,
}

fn load_skill_from_file(file_path: &str, source: &str) -> LoadedSkill {
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    let raw_content = match std::fs::read_to_string(file_path) {
        Ok(content) => content,
        Err(error) => {
            let message = if error.to_string().is_empty() {
                "failed to parse skill file".to_string()
            } else {
                error.to_string()
            };
            diagnostics.push(warning(message, file_path));
            return LoadedSkill {
                skill: None,
                diagnostics,
            };
        }
    };
    let parsed = match parse_frontmatter(&raw_content) {
        Ok(parsed) => parsed,
        Err(error) => {
            diagnostics.push(warning(error.to_string(), file_path));
            return LoadedSkill {
                skill: None,
                diagnostics,
            };
        }
    };
    let frontmatter: SkillFrontmatter = match serde_json::from_value(parsed.frontmatter.clone()) {
        Ok(value) => value,
        Err(error) => {
            diagnostics.push(warning(error.to_string(), file_path));
            return LoadedSkill {
                skill: None,
                diagnostics,
            };
        }
    };
    let skill_dir = Path::new(file_path).parent().unwrap_or(Path::new(""));
    let parent_dir_name = skill_dir
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();

    for error in validate_description(frontmatter.description.as_deref()) {
        diagnostics.push(warning(error, file_path));
    }
    let name = frontmatter
        .name
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| parent_dir_name.clone());
    for error in validate_name(&name, &parent_dir_name) {
        diagnostics.push(warning(error, file_path));
    }
    if frontmatter
        .description
        .as_deref()
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
    {
        return LoadedSkill {
            skill: None,
            diagnostics,
        };
    }
    let python = if Path::new(file_path)
        .file_name()
        .map(|value| value.to_string_lossy() == "SKILL.md")
        .unwrap_or(false)
    {
        detect_python_skill(skill_dir, &name, &mut diagnostics)
    } else {
        None
    };
    let base = BaseSkill {
        name: name.clone(),
        description: frontmatter.description.clone().unwrap_or_default(),
        file_path: file_path.to_string(),
        base_dir: skill_dir.to_string_lossy().to_string(),
        source_info: create_skill_source_info(file_path, &skill_dir.to_string_lossy(), source),
        disable_model_invocation: frontmatter.disable_model_invocation,
    };
    let skill = match python {
        Some(python) => Skill::Python(PythonSkill {
            base,
            kind: SkillKind::Python,
            python,
        }),
        None => Skill::Markdown(MarkdownSkill {
            base,
            kind: SkillKind::Markdown,
        }),
    };
    LoadedSkill {
        skill: Some(skill),
        diagnostics,
    }
}

/// Format skills for inclusion in a system prompt.
/// Uses XML format per Agent Skills standard.
/// See: https://agentskills.io/integrate-skills
///
/// Skills with disableModelInvocation=true are excluded from the prompt
/// (they can only be invoked explicitly via /skill:name commands).
pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    let visible: Vec<&Skill> = skills
        .iter()
        .filter(|skill| match skill {
            Skill::Markdown(value) => !value.base.disable_model_invocation,
            Skill::Python(value) => !value.base.disable_model_invocation,
        })
        .collect();
    if visible.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = vec![
        "\n\nThe following skills provide specialized instructions for specific tasks.".to_string(),
        "Use ipython to inspect a skill's file when the task matches its description.".to_string(),
        "Skills with a python_import are prepared in the persistent Python kernel when available and can be called directly by that import name.".to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];
    for skill in visible {
        let (base, kind, python_import) = match skill {
            Skill::Markdown(value) => (&value.base, SkillKind::Markdown, None),
            Skill::Python(value) => (
                &value.base,
                SkillKind::Python,
                Some(value.python.import_name.clone()),
            ),
        };
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&base.name)));
        lines.push(format!("    <type>{}</type>", kind.as_str()));
        if let Some(python_import) = python_import {
            lines.push(format!(
                "    <python_import>{}</python_import>",
                escape_xml(&python_import)
            ));
        }
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&base.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&base.file_path)
        ));
        lines.push("  </skill>".to_string());
    }
    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[derive(Debug, Clone)]
pub struct LoadSkillsOptions {
    /// Working directory for project-local skills.
    pub cwd: String,
    /// Agent config directory for global skills.
    pub agent_dir: String,
    /// Explicit skill paths (files or directories)
    pub skill_paths: Vec<String>,
    /// Include default skills directories.
    pub include_defaults: bool,
}

fn normalize_path(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed == "~" {
        return home_dir();
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return Path::new(&home_dir())
            .join(rest)
            .to_string_lossy()
            .to_string();
    }
    if let Some(rest) = trimmed.strip_prefix('~') {
        return Path::new(&home_dir())
            .join(rest)
            .to_string_lossy()
            .to_string();
    }
    trimmed.to_string()
}

fn home_dir() -> String {
    dirs::home_dir()
        .map(|home| home.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn resolve_skill_path(path: &str, cwd: &str) -> String {
    let normalized = normalize_path(path);
    if Path::new(&normalized).is_absolute() {
        normalized
    } else {
        Path::new(cwd)
            .join(normalized)
            .to_string_lossy()
            .to_string()
    }
}

/// `CONFIG_DIR_NAME` from config.ts.
/// blocked_on: needs crate::config::CONFIG_DIR_NAME
pub const CONFIG_DIR_NAME: &str = ".prime/agent";

/// Load skills from all configured locations.
/// Returns skills and any validation diagnostics.
pub fn load_skills(options: &LoadSkillsOptions) -> LoadSkillsResult {
    let resolved_agent_dir = if options.agent_dir.is_empty() {
        crate::core::memory::service::get_agent_dir()
    } else {
        options.agent_dir.clone()
    };
    let mut skill_map: IndexMap<String, Skill> = IndexMap::new();
    let mut real_path_set: Vec<String> = Vec::new();
    let mut python_import_map: IndexMap<String, Skill> = IndexMap::new();
    let mut all_diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    let mut collision_diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    let mut python_import_diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    fn add_skills(
        result: LoadSkillsResult,
        skill_map: &mut IndexMap<String, Skill>,
        real_path_set: &mut Vec<String>,
        python_import_map: &mut IndexMap<String, Skill>,
        all_diagnostics: &mut Vec<ResourceDiagnostic>,
        collision_diagnostics: &mut Vec<ResourceDiagnostic>,
        python_import_diagnostics: &mut Vec<ResourceDiagnostic>,
    ) {
        all_diagnostics.extend(result.diagnostics);
        for skill in result.skills {
            let real_path = canonicalize_path(skill.file_path());
            if real_path_set.contains(&real_path) {
                continue;
            }
            let name = skill.name().to_string();
            match skill_map.get(&name) {
                Some(existing) => collision_diagnostics.push(ResourceDiagnostic {
                    diagnostic_type: crate::core::diagnostics::RESOURCE_DIAGNOSTIC_COLLISION
                        .to_string(),
                    message: format!("name \"{name}\" collision"),
                    path: Some(skill.file_path().to_string()),
                    collision: Some(crate::core::diagnostics::ResourceCollision {
                        resource_type: crate::core::diagnostics::RESOURCE_TYPE_SKILL.to_string(),
                        name: name.clone(),
                        winner_path: existing.file_path().to_string(),
                        loser_path: skill.file_path().to_string(),
                        winner_source: None,
                        loser_source: None,
                    }),
                }),
                None => {
                    skill_map.insert(name, skill.clone());
                    real_path_set.push(real_path);
                    if let Skill::Python(python) = &skill {
                        match python_import_map.get(&python.python.import_name) {
                            Some(existing) => python_import_diagnostics.push(warning(
                                format!(
                                    "python import name \"{}\" is shared by skills \"{}\" and \"{}\"",
                                    python.python.import_name,
                                    existing.name(),
                                    python.base.name
                                ),
                                &python.base.file_path,
                            )),
                            None => {
                                python_import_map
                                    .insert(python.python.import_name.clone(), skill.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    let user_skills_dir = Path::new(&resolved_agent_dir)
        .join("skills")
        .to_string_lossy()
        .to_string();
    let project_skills_dir = Path::new(&options.cwd)
        .join(CONFIG_DIR_NAME)
        .join("skills")
        .to_string_lossy()
        .to_string();

    if options.include_defaults {
        let user = load_skills_from_dir_internal(&user_skills_dir, "user", true, None, None);
        add_skills(
            user,
            &mut skill_map,
            &mut real_path_set,
            &mut python_import_map,
            &mut all_diagnostics,
            &mut collision_diagnostics,
            &mut python_import_diagnostics,
        );
        let project =
            load_skills_from_dir_internal(&project_skills_dir, "project", true, None, None);
        add_skills(
            project,
            &mut skill_map,
            &mut real_path_set,
            &mut python_import_map,
            &mut all_diagnostics,
            &mut collision_diagnostics,
            &mut python_import_diagnostics,
        );
    }

    let is_under_path = |target: &str, root: &str| -> bool {
        let normalized_root = Path::new(root);
        let target_path = Path::new(target);
        if target_path == normalized_root {
            return true;
        }
        target_path.starts_with(normalized_root)
    };

    let get_source = |resolved_path: &str| -> &'static str {
        if !options.include_defaults {
            if is_under_path(resolved_path, &user_skills_dir) {
                return "user";
            }
            if is_under_path(resolved_path, &project_skills_dir) {
                return "project";
            }
        }
        "path"
    };

    for raw_path in &options.skill_paths {
        let resolved_path = resolve_skill_path(raw_path, &options.cwd);
        if !Path::new(&resolved_path).exists() {
            all_diagnostics.push(warning(
                "skill path does not exist".to_string(),
                &resolved_path,
            ));
            continue;
        }
        let source = get_source(&resolved_path);
        match std::fs::metadata(&resolved_path) {
            Ok(metadata) if metadata.is_dir() => {
                let result =
                    load_skills_from_dir_internal(&resolved_path, source, true, None, None);
                add_skills(
                    result,
                    &mut skill_map,
                    &mut real_path_set,
                    &mut python_import_map,
                    &mut all_diagnostics,
                    &mut collision_diagnostics,
                    &mut python_import_diagnostics,
                );
            }
            Ok(metadata) if metadata.is_file() && resolved_path.ends_with(".md") => {
                let result = load_skill_from_file(&resolved_path, source);
                match result.skill {
                    Some(skill) => add_skills(
                        LoadSkillsResult {
                            skills: vec![skill],
                            diagnostics: result.diagnostics,
                        },
                        &mut skill_map,
                        &mut real_path_set,
                        &mut python_import_map,
                        &mut all_diagnostics,
                        &mut collision_diagnostics,
                        &mut python_import_diagnostics,
                    ),
                    None => all_diagnostics.extend(result.diagnostics),
                }
            }
            Ok(_) => all_diagnostics.push(warning(
                "skill path is not a markdown file".to_string(),
                &resolved_path,
            )),
            Err(error) => all_diagnostics.push(warning(error.to_string(), &resolved_path)),
        }
    }

    let mut diagnostics = all_diagnostics;
    diagnostics.extend(collision_diagnostics);
    diagnostics.extend(python_import_diagnostics);
    LoadSkillsResult {
        skills: skill_map.into_values().collect(),
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prime-skills-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_skill(root: &Path, name: &str, frontmatter: &str, body: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("SKILL.md");
        std::fs::write(&file, format!("---\n{frontmatter}\n---\n\n{body}\n")).unwrap();
        file
    }

    #[test]
    fn loads_a_valid_skill_and_reports_no_diagnostics() {
        let root = temp_dir();
        write_skill(
            &root,
            "websearch",
            "name: websearch\ndescription: Search the web",
            "Body",
        );
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: root.to_string_lossy().to_string(),
            source: "user".to_string(),
        });
        assert_eq!(result.skills.len(), 1);
        assert!(result.diagnostics.is_empty());
        let skill = &result.skills[0];
        assert_eq!(skill.name(), "websearch");
        assert_eq!(skill.kind(), SkillKind::Markdown);
        match skill {
            Skill::Markdown(value) => {
                assert_eq!(value.base.description, "Search the web");
                assert_eq!(value.base.source_info.scope, "user");
                assert_eq!(value.base.source_info.source, "local");
                assert!(!value.base.disable_model_invocation);
            }
            _ => panic!("expected a markdown skill"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn warns_when_the_name_does_not_match_the_parent_directory() {
        let root = temp_dir();
        write_skill(&root, "websearch", "name: other\ndescription: d", "Body");
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: root.to_string_lossy().to_string(),
            source: "user".to_string(),
        });
        assert_eq!(
            result.diagnostics[0].message,
            "name \"other\" does not match parent directory \"websearch\""
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn warns_for_invalid_names_and_skips_missing_descriptions() {
        let root = temp_dir();
        write_skill(&root, "bad_name", "name: bad_name\ndescription: d", "Body");
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: root.to_string_lossy().to_string(),
            source: "user".to_string(),
        });
        assert!(result.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("name contains invalid characters")));
        let long = "a".repeat(65);
        write_skill(
            &root,
            "long",
            &format!("name: {long}\ndescription: d"),
            "Body",
        );
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: root.to_string_lossy().to_string(),
            source: "user".to_string(),
        });
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message
                == format!("name exceeds 64 characters ({})", long.len())));
        let missing = root.join("nodesc");
        std::fs::create_dir_all(&missing).unwrap();
        std::fs::write(missing.join("SKILL.md"), "---\nname: nodesc\n---\n\nBody\n").unwrap();
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: root.to_string_lossy().to_string(),
            source: "user".to_string(),
        });
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message == "description is required"));
        assert!(!result.skills.iter().any(|skill| skill.name() == "nodesc"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn warns_for_consecutive_hyphens_and_hyphen_edges() {
        let root = temp_dir();
        write_skill(&root, "a--b", "name: a--b\ndescription: d", "Body");
        write_skill(&root, "-lead", "name: -lead\ndescription: d", "Body");
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: root.to_string_lossy().to_string(),
            source: "user".to_string(),
        });
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message == "name must not contain consecutive hyphens"));
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message == "name must not start or end with a hyphen"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn loads_nested_skills_recursively_but_prefers_a_root_skill_md() {
        let root = temp_dir();
        write_skill(&root, "nested", "name: nested\ndescription: d", "Body");
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: root.to_string_lossy().to_string(),
            source: "user".to_string(),
        });
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name(), "nested");
        // A directory with SKILL.md is a skill root: nested SKILL.md files are not loaded.
        let root_file = write_skill(&root, "outer", "name: outer\ndescription: d", "Body");
        write_skill(
            &root.join("outer"),
            "inner",
            "name: inner\ndescription: d",
            "Body",
        );
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: root_file.parent().unwrap().to_string_lossy().to_string(),
            source: "user".to_string(),
        });
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name(), "outer");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn skips_files_without_frontmatter_and_warns_on_invalid_yaml() {
        let root = temp_dir();
        let plain = root.join("plain.md");
        std::fs::write(&plain, "no frontmatter").unwrap();
        let invalid = root.join("invalid.md");
        std::fs::write(&invalid, "---\nname: [unclosed\n---\n\nBody\n").unwrap();
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: root.to_string_lossy().to_string(),
            source: "user".to_string(),
        });
        assert!(result.skills.is_empty());
        assert!(!result.diagnostics.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn parses_disable_model_invocation_and_ignores_unknown_fields() {
        let root = temp_dir();
        write_skill(
            &root,
            "quiet",
            "name: quiet\ndescription: d\ndisable-model-invocation: true\nunknown: 1",
            "Body",
        );
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: root.to_string_lossy().to_string(),
            source: "user".to_string(),
        });
        match &result.skills[0] {
            Skill::Markdown(value) => assert!(value.base.disable_model_invocation),
            _ => panic!("expected markdown"),
        }
        assert_eq!(format_skills_for_prompt(&result.skills), "");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn returns_empty_for_a_missing_directory() {
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: std::env::temp_dir()
                .join("prime-skills-missing")
                .to_string_lossy()
                .to_string(),
            source: "user".to_string(),
        });
        assert!(result.skills.is_empty());
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn detects_python_skills_and_warns_when_the_package_is_missing() {
        let root = temp_dir();
        let skill_dir = root.join("websearch");
        std::fs::create_dir_all(skill_dir.join("src").join("websearch")).unwrap();
        std::fs::write(
            skill_dir.join("pyproject.toml"),
            "[project]\nname = \"websearch\"\n",
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("src").join("websearch").join("__init__.py"),
            "",
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: websearch\ndescription: d\n---\n\nBody\n",
        )
        .unwrap();
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: root.to_string_lossy().to_string(),
            source: "user".to_string(),
        });
        match &result.skills[0] {
            Skill::Python(value) => {
                assert_eq!(value.python.import_name, "websearch");
                assert!(value.python.pyproject_path.ends_with("pyproject.toml"));
            }
            _ => panic!("expected python skill"),
        }
        let info = get_python_skill_runtime_info(&result.skills);
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].name, "websearch");
        // Missing package files degrade to a markdown skill with a warning.
        std::fs::remove_dir_all(skill_dir.join("src")).unwrap();
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: root.to_string_lossy().to_string(),
            source: "user".to_string(),
        });
        assert_eq!(result.skills[0].kind(), SkillKind::Markdown);
        assert!(result.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("python skill package src/websearch/__init__.py not found")));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn formats_skills_as_xml_with_python_import_and_escaping() {
        assert_eq!(format_skills_for_prompt(&[]), "");
        let root = temp_dir();
        write_skill(
            &root,
            "a-b",
            "name: a-b\ndescription: Use <x> & \"y\"",
            "Body",
        );
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: root.to_string_lossy().to_string(),
            source: "user".to_string(),
        });
        let text = format_skills_for_prompt(&result.skills);
        assert!(text.starts_with(
            "\n\nThe following skills provide specialized instructions for specific tasks."
        ));
        assert!(text.contains("<available_skills>"));
        assert!(text.contains("    <name>a-b</name>"));
        assert!(text.contains("    <type>markdown</type>"));
        assert!(text.contains("<description>Use &lt;x&gt; &amp; &quot;y&quot;</description>"));
        assert!(text.ends_with("</available_skills>"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_skills_warns_for_missing_paths_and_expands_tilde() {
        let root = temp_dir();
        let result = load_skills(&LoadSkillsOptions {
            cwd: root.to_string_lossy().to_string(),
            agent_dir: root.join("agent").to_string_lossy().to_string(),
            skill_paths: vec!["~/definitely-missing-prime-skill".to_string()],
            include_defaults: false,
        });
        assert!(result.skills.is_empty());
        assert_eq!(result.diagnostics[0].message, "skill path does not exist");
        assert!(result.diagnostics[0]
            .path
            .as_deref()
            .unwrap()
            .starts_with(&home_dir()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_skills_detects_collisions_and_shared_python_import_names() {
        let root = temp_dir();
        let agent_dir = root.join("agent");
        let cwd = root.join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        write_skill(
            &agent_dir.join("skills"),
            "dup",
            "name: dup\ndescription: first",
            "Body",
        );
        let explicit = root.join("explicit").join("dup");
        std::fs::create_dir_all(&explicit).unwrap();
        std::fs::write(
            explicit.join("SKILL.md"),
            "---\nname: dup\ndescription: second\n---\n\nBody\n",
        )
        .unwrap();
        let result = load_skills(&LoadSkillsOptions {
            cwd: cwd.to_string_lossy().to_string(),
            agent_dir: agent_dir.to_string_lossy().to_string(),
            skill_paths: vec![root.join("explicit").to_string_lossy().to_string()],
            include_defaults: true,
        });
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].file_path().contains("agent"), true);
        let collision = result
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.diagnostic_type == "collision")
            .expect("collision");
        assert_eq!(collision.message, "name \"dup\" collision");
        assert_eq!(
            collision
                .collision
                .as_ref()
                .unwrap()
                .winner_path
                .contains("agent"),
            true
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_skills_warns_when_python_import_names_are_shared() {
        let root = temp_dir();
        let cwd = root.join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        for (directory, name) in [("one", "shared-name"), ("two", "shared_name")] {
            let skill_dir = root.join(directory).join(name);
            std::fs::create_dir_all(skill_dir.join("src").join("shared_name")).unwrap();
            std::fs::write(skill_dir.join("pyproject.toml"), "[project]\n").unwrap();
            std::fs::write(
                skill_dir
                    .join("src")
                    .join("shared_name")
                    .join("__init__.py"),
                "",
            )
            .unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: d\n---\n\nBody\n"),
            )
            .unwrap();
        }
        let result = load_skills(&LoadSkillsOptions {
            cwd: cwd.to_string_lossy().to_string(),
            agent_dir: root.join("agent").to_string_lossy().to_string(),
            skill_paths: vec![root.to_string_lossy().to_string()],
            include_defaults: false,
        });
        assert!(result.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("python import name \"shared_name\" is shared by skills")));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_skills_warns_when_a_path_is_not_markdown() {
        let root = temp_dir();
        let file = root.join("notes.txt");
        std::fs::write(&file, "x").unwrap();
        let result = load_skills(&LoadSkillsOptions {
            cwd: root.to_string_lossy().to_string(),
            agent_dir: root.join("agent").to_string_lossy().to_string(),
            skill_paths: vec![file.to_string_lossy().to_string()],
            include_defaults: false,
        });
        assert_eq!(
            result.diagnostics[0].message,
            "skill path is not a markdown file"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn ignore_matcher_honours_negation_and_directory_patterns() {
        let mut matcher = IgnoreMatcher::new();
        matcher.add(&[
            "node_modules/".to_string(),
            "*.md".to_string(),
            "!keep.md".to_string(),
        ]);
        assert!(matcher.ignores("node_modules/"));
        assert!(matcher.ignores("a/b.md"));
        assert!(!matcher.ignores("keep.md"));
        let mut prefixed = IgnoreMatcher::new();
        prefixed.add(&["sub/*.md".to_string()]);
        assert!(prefixed.ignores("sub/x.md"));
        assert!(!prefixed.ignores("other/x.md"));
    }
}
