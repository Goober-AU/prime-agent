//! Port of packages/coding-agent/src/core/tools/edit.ts

use std::sync::Arc;

use pi_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback};
use pi_agent_core::types::ContentBlock;
use pi_tui::utils::wrap_text_with_ansi;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{ExtensionContext, ToolDefinition};

use super::edit_diff::{
    apply_edits_to_normalized_content, compute_edits_diff, detect_line_ending, generate_diff_string,
    normalize_to_lf, restore_line_endings, strip_bom, Edit, EditDiffOutcome, AppliedEditsResult,
};
use super::file_mutation_queue::with_file_mutation_queue;
use super::path_utils::resolve_to_cwd;
use super::render_utils::{invalid_arg_text, shorten_path, str_value, ToolTheme};
use super::tool_definition_wrapper::wrap_tool_definition;

/// TypeScript `EditPreview = EditDiffResult | EditDiffError`.
#[derive(Debug, Clone, PartialEq)]
pub enum EditPreview {
    Result {
        diff: String,
        first_changed_line: Option<usize>,
    },
    Error {
        error: String,
    },
}

impl From<EditDiffOutcome> for EditPreview {
    fn from(outcome: EditDiffOutcome) -> Self {
        match outcome {
            EditDiffOutcome::Result(result) => EditPreview::Result {
                diff: result.diff,
                first_changed_line: result.first_changed_line,
            },
            EditDiffOutcome::Error(error) => EditPreview::Error { error: error.error },
        }
    }
}

/// TypeScript `type EditRenderState = { callComponent?: EditCallRenderComponent }`.
#[derive(Default)]
pub struct EditRenderState {
    pub call_component: Option<EditCallRenderComponent>,
}

/// TypeScript `type EditCallRenderComponent = Box & { preview?, previewArgsKey?, previewPending?, settledError? }`.
pub struct EditCallRenderComponent {
    pub preview: Option<EditPreview>,
    pub preview_args_key: Option<String>,
    pub preview_pending: bool,
    pub settled_error: bool,
    /// Box padding `(1, 1, bgFn)` from `new Box(1, 1, (text) => text)`.
    pub box_padding_x: usize,
    pub box_padding_y: usize,
    pub children: Vec<String>,
}

impl EditCallRenderComponent {
    pub fn new() -> Self {
        Self {
            preview: None,
            preview_args_key: None,
            preview_pending: false,
            settled_error: false,
            box_padding_x: 1,
            box_padding_y: 1,
            children: Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }

    pub fn add_child(&mut self, text: String) {
        self.children.push(text);
    }

    pub fn set_bg_fn(&mut self, _bg: Box<dyn Fn(&str) -> String>) {}
}

/// Port of the edit tool's TypeBox schema.
pub const EDIT_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "path": { "type": "string", "description": "Path to the file to edit (relative or absolute)" },
    "edits": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "oldText": {
            "type": "string",
            "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."
          },
          "newText": { "type": "string", "description": "Replacement text for this targeted edit." }
        },
        "required": ["oldText", "newText"],
        "additionalProperties": false
      },
      "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead."
    }
  },
  "required": ["path", "edits"],
  "additionalProperties": false
}"#;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditToolInput {
    pub path: String,
    pub edits: Vec<Edit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditToolDetails {
    /// Unified diff of the changes made
    pub diff: String,
    /// Line number of the first change in the new file (for editor navigation)
    #[serde(rename = "firstChangedLine", skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<usize>,
}

/// Pluggable operations for the edit tool.
/// Override these to delegate file editing to remote systems (for example SSH).
pub trait EditOperations: Send + Sync {
    /// Read file contents as a Buffer
    fn read_file(&self, absolute_path: &str) -> std::io::Result<Vec<u8>>;
    /// Write content to a file
    fn write_file(&self, absolute_path: &str, content: &str) -> std::io::Result<()>;
    /// Check if file is readable and writable (throw if not)
    fn access(&self, absolute_path: &str) -> std::io::Result<()>;
}

/// Default local-filesystem operations (`fsAccess(path, R_OK | W_OK)`).
#[derive(Debug, Default)]
pub struct LocalEditOperations;

impl EditOperations for LocalEditOperations {
    fn read_file(&self, absolute_path: &str) -> std::io::Result<Vec<u8>> {
        std::fs::read(absolute_path)
    }

    fn write_file(&self, absolute_path: &str, content: &str) -> std::io::Result<()> {
        std::fs::write(absolute_path, content)
    }

    fn access(&self, absolute_path: &str) -> std::io::Result<()> {
        let path = std::path::Path::new(absolute_path);
        let metadata = std::fs::metadata(path)?;
        if metadata.permissions().readonly() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "file is read-only",
            ));
        }
        Ok(())
    }
}

/// TypeScript `interface EditToolOptions`.
#[derive(Clone, Default)]
pub struct EditToolOptions {
    /// Custom operations for file editing. Default: local filesystem
    pub operations: Option<Arc<dyn EditOperations>>,
}

impl std::fmt::Debug for EditToolOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditToolOptions")
            .field("operations", &self.operations.as_ref().map(|_| "EditOperations"))
            .finish()
    }
}

/// TypeScript `prepareEditArguments(input: unknown)`.
pub fn prepare_edit_arguments(input: Value) -> Value {
    let Value::Object(mut args) = input else {
        return input;
    };

    // Some models (Opus 4.6, GLM-5.1) send edits as a JSON string instead of an array
    if let Some(Value::String(raw)) = args.get("edits") {
        if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
            if parsed.is_array() {
                args.insert("edits".to_string(), parsed);
            }
        }
    }

    let legacy_old_text = args.get("oldText").cloned();
    let legacy_new_text = args.get("newText").cloned();
    let (Some(Value::String(old_text)), Some(Value::String(new_text))) = (legacy_old_text, legacy_new_text) else {
        return Value::Object(args);
    };

    let mut edits: Vec<Value> = match args.get("edits") {
        Some(Value::Array(existing)) => existing.clone(),
        _ => Vec::new(),
    };
    edits.push(serde_json::json!({ "oldText": old_text, "newText": new_text }));
    let mut rest = args;
    rest.shift_remove("oldText");
    rest.shift_remove("newText");
    rest.insert("edits".to_string(), Value::Array(edits));
    Value::Object(rest)
}

/// TypeScript `validateEditInput`.
pub fn validate_edit_input(input: &EditToolInput) -> Result<(String, Vec<Edit>), String> {
    if input.edits.is_empty() {
        return Err("Edit tool input is invalid. edits must contain at least one replacement.".to_string());
    }
    Ok((input.path.clone(), input.edits.clone()))
}

/// TypeScript `type RenderableEditArgs`.
#[derive(Debug, Clone, Default)]
pub struct RenderableEditArgs {
    pub path: Option<String>,
    pub file_path: Option<String>,
    pub edits: Option<Vec<Edit>>,
    pub old_text: Option<String>,
    pub new_text: Option<String>,
}

impl RenderableEditArgs {
    pub fn from_value(value: Option<&Value>) -> Self {
        let Some(Value::Object(map)) = value else {
            return Self::default();
        };
        let edits = map.get("edits").and_then(|edits| {
            serde_json::from_value::<Vec<Edit>>(edits.clone()).ok()
        });
        Self {
            path: map.get("path").and_then(|path| path.as_str()).map(str::to_string),
            file_path: map.get("file_path").and_then(|path| path.as_str()).map(str::to_string),
            edits,
            old_text: map.get("oldText").and_then(|text| text.as_str()).map(str::to_string),
            new_text: map.get("newText").and_then(|text| text.as_str()).map(str::to_string),
        }
    }

    /// `str(args?.file_path ?? args?.path)`.
    pub fn raw_path(&self) -> Option<String> {
        match (&self.file_path, &self.path) {
            (Some(file_path), _) => Some(file_path.clone()),
            (None, Some(path)) => Some(path.clone()),
            _ => None,
        }
    }
}

pub struct EditToolResultLike {
    pub content: Vec<super::render_utils::RenderContentBlock>,
    pub details: Option<EditToolDetails>,
}

fn get_edit_call_render_component(
    state: &mut EditRenderState,
    last_component: Option<EditCallRenderComponent>,
) -> EditCallRenderComponent {
    if let Some(component) = last_component {
        state.call_component = Some(component);
        return state.call_component.clone().unwrap_or_default();
    }
    if let Some(component) = state.call_component.as_ref() {
        return component.clone();
    }
    let component = EditCallRenderComponent::new();
    state.call_component = Some(component.clone());
    component
}

impl Default for EditCallRenderComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for EditCallRenderComponent {
    fn clone(&self) -> Self {
        Self {
            preview: self.preview.clone(),
            preview_args_key: self.preview_args_key.clone(),
            preview_pending: self.preview_pending,
            settled_error: self.settled_error,
            box_padding_x: self.box_padding_x,
            box_padding_y: self.box_padding_y,
            children: self.children.clone(),
        }
    }
}

/// TypeScript `getRenderablePreviewInput`.
pub fn get_renderable_preview_input(args: Option<&RenderableEditArgs>) -> Option<(String, Vec<Edit>)> {
    let args = args?;

    let path = match (&args.path, &args.file_path) {
        (Some(path), _) => path.clone(),
        (None, Some(file_path)) => file_path.clone(),
        _ => return None,
    };

    if let Some(edits) = args.edits.as_ref() {
        if !edits.is_empty() {
            return Some((path, edits.clone()));
        }
    }

    match (&args.old_text, &args.new_text) {
        (Some(old_text), Some(new_text)) => Some((
            path,
            vec![Edit {
                old_text: old_text.clone(),
                new_text: new_text.clone(),
            }],
        )),
        _ => None,
    }
}

fn format_edit_call(args: Option<&RenderableEditArgs>, theme: &dyn ToolTheme) -> String {
    let invalid_arg = invalid_arg_text(theme);
    let raw_path = args.and_then(|args| args.raw_path());
    let path = raw_path.as_deref().map(|raw| shorten_path(&Value::String(raw.to_string())));
    let path_display = match path {
        None => invalid_arg,
        Some(path) if !path.is_empty() => theme.fg("accent", &path),
        Some(_) => theme.fg("toolOutput", "..."),
    };
    format!("{} {}", theme.fg("toolTitle", &theme.bold("edit")), path_display)
}

fn format_edit_result(
    args: Option<&RenderableEditArgs>,
    preview: Option<&EditPreview>,
    result: &EditToolResultLike,
    theme: &dyn ToolTheme,
    is_error: bool,
) -> Option<String> {
    let raw_path = args.and_then(|args| args.raw_path());
    let preview_diff = match preview {
        Some(EditPreview::Result { diff, .. }) => Some(diff.clone()),
        _ => None,
    };
    let preview_error = match preview {
        Some(EditPreview::Error { error }) => Some(error.clone()),
        _ => None,
    };
    if is_error {
        let error_text = result
            .content
            .iter()
            .filter(|block| block.r#type == "text")
            .map(|block| block.text.clone().unwrap_or_default())
            .collect::<Vec<String>>()
            .join("\n");
        if error_text.is_empty() || Some(error_text.clone()) == preview_error {
            return None;
        }
        return Some(theme.fg("error", &error_text));
    }

    let result_diff = result.details.as_ref().map(|details| details.diff.clone());
    if let Some(result_diff) = result_diff {
        if Some(result_diff.clone()) != preview_diff {
            return Some(render_diff(&result_diff, raw_path.as_deref()));
        }
    }

    None
}

fn get_edit_header_bg(preview: Option<&EditPreview>, settled_error: bool, theme: &dyn ToolTheme) -> Box<dyn Fn(&str) -> String> {
    if settled_error || matches!(preview, Some(EditPreview::Error { .. })) {
        let theme_bg = theme.bg("toolErrorBg", "");
        let _ = theme_bg;
        return Box::new(|_text: &str| String::new());
    }
    if preview.is_some() {
        return Box::new(|_text: &str| String::new());
    }
    Box::new(|_text: &str| String::new())
}

/// Port of modes/interactive/components/diff.ts `renderDiff` (plain rendering).
fn render_diff(diff: &str, file_path: Option<&str>) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(file_path) = file_path {
        lines.push(file_path.to_string());
    }
    for line in diff.split('\n') {
        if line.starts_with('+') {
            lines.push(format!("\u{1b}[32m{line}\u{1b}[0m"));
        } else if line.starts_with('-') {
            lines.push(format!("\u{1b}[31m{line}\u{1b}[0m"));
        } else {
            lines.push(line.to_string());
        }
    }
    lines.join("\n")
}

const FILE_CHANGE_DIFF_INDENT: &str = "  ";

/// Port of modes/interactive/components/edit-summary.ts `countChangedLines`.
pub fn count_changed_lines(diff: &str) -> (usize, usize) {
    let mut added = 0usize;
    let mut removed = 0usize;
    for line in diff.split('\n') {
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

/// Port of modes/interactive/components/edit-summary.ts `formatFileChangeSummaryLine`.
pub fn format_file_change_summary_line(
    raw_path: &str,
    cwd: &str,
    change: (usize, usize),
    diffs_expanded: bool,
    width: usize,
) -> String {
    let display = shorten_path(&Value::String(raw_path.to_string()));
    let relative = match std::path::Path::new(raw_path).strip_prefix(cwd) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative.to_string_lossy().into_owned(),
        _ => display,
    };
    let (added, removed) = change;
    let expanded = if diffs_expanded { " \u{25be}" } else { " \u{25b8}" };
    let summary = format!("\u{2570}\u{2500} {relative} +{added} -{removed}{expanded}");
    if summary.chars().count() <= width {
        summary
    } else {
        let head: String = summary.chars().take(width.saturating_sub(3)).collect();
        format!("{head}...")
    }
}

fn build_edit_call_component(
    component: &mut EditCallRenderComponent,
    args: Option<&RenderableEditArgs>,
    theme: &dyn ToolTheme,
    expanded: bool,
    cwd: &str,
) {
    let _ = get_edit_header_bg(component.preview.as_ref(), component.settled_error, theme);
    component.clear();
    component.add_child(format_edit_call(args, theme));

    let preview_error = match component.preview.as_ref() {
        Some(EditPreview::Error { error }) => Some(error.clone()),
        _ => None,
    };
    if let Some(error) = preview_error {
        component.add_child(String::new());
        component.add_child(theme.fg("error", &error));
        return;
    }
    // A failed execution must not present the predicted diff as applied changes.
    let Some(preview) = component.preview.as_ref() else {
        return;
    };
    if component.settled_error {
        return;
    }

    // The summary line renders in both states; ctrl+j only attaches or removes
    // the indented diff lines underneath it.
    let raw_path = args.and_then(|args| args.raw_path());
    let (diff, _) = match preview {
        EditPreview::Result { diff, first_changed_line } => (diff.clone(), *first_changed_line),
        EditPreview::Error { .. } => return,
    };
    let change = count_changed_lines(&diff);
    component.add_child(String::new());
    let safe_width = 80usize;
    let mut summary_lines = vec![format_file_change_summary_line(
        raw_path.as_deref().unwrap_or("..."),
        cwd,
        change,
        expanded,
        safe_width,
    )];
    if expanded {
        let rendered = render_diff(&diff, None);
        let indent = &FILE_CHANGE_DIFF_INDENT[..std::cmp::min(FILE_CHANGE_DIFF_INDENT.len(), safe_width.saturating_sub(1))];
        let content_width = std::cmp::max(1, safe_width - indent.chars().count());
        for line in rendered.split('\n') {
            for row in wrap_text_with_ansi(line, content_width) {
                summary_lines.push(format!("{indent}{row}"));
            }
        }
    }
    component.add_child(summary_lines.join("\n"));
}

fn set_edit_preview(
    component: &mut EditCallRenderComponent,
    preview: EditPreview,
    args_key: Option<String>,
) -> bool {
    let current = component.preview.clone();
    let changed = match (&current, &preview) {
        (None, _) => true,
        (Some(EditPreview::Error { error: previous }), EditPreview::Error { error: next }) => previous != next,
        (Some(EditPreview::Error { .. }), _) => true,
        (Some(EditPreview::Result { .. }), EditPreview::Error { .. }) => true,
        (
            Some(EditPreview::Result {
                diff: previous_diff,
                first_changed_line: previous_line,
            }),
            EditPreview::Result {
                diff: next_diff,
                first_changed_line: next_line,
            },
        ) => previous_diff != next_diff || previous_line != next_line,
    };
    component.preview = Some(preview);
    component.preview_args_key = args_key;
    component.preview_pending = false;
    changed
}

fn preview_args_key(preview_input: Option<&(String, Vec<Edit>)>) -> Option<String> {
    preview_input.map(|(path, edits)| {
        serde_json::json!({
            "path": path,
            "edits": edits.iter().map(|edit| serde_json::json!({
                "oldText": edit.old_text,
                "newText": edit.new_text,
            })).collect::<Vec<Value>>(),
        })
        .to_string()
    })
}

/// Port of the edit tool's `execute` body.
pub async fn execute_edit(
    cwd: &str,
    operations: Option<Arc<dyn EditOperations>>,
    input: &EditToolInput,
    signal: Option<CancellationToken>,
) -> Result<(String, EditToolDetails), String> {
    let (path, edits) = validate_edit_input(input)?;
    let absolute_path = resolve_to_cwd(&path, cwd);
    let operations = operations.unwrap_or_else(|| Arc::new(LocalEditOperations));
    let path_for_task = path.clone();
    let absolute_for_task = absolute_path.clone();

    let result = with_file_mutation_queue(&absolute_path, || {
        let operations = operations.clone();
        let path = path_for_task.clone();
        let absolute_path = absolute_for_task.clone();
        let signal = signal.clone();
        async move {
            if signal.as_ref().map(|token| token.is_cancelled()).unwrap_or(false) {
                return Err("Operation aborted".to_string());
            }

            let access = tokio::task::spawn_blocking({
                let operations = operations.clone();
                let absolute_path = absolute_path.clone();
                move || operations.access(&absolute_path)
            })
            .await
            .map_err(|error| error.to_string())?;
            if let Err(error) = access {
                let error_message = format!("Error code: {}", io_error_code(&error));
                return Err(format!("Could not edit file: {path}. {error_message}."));
            }

            let buffer = tokio::task::spawn_blocking({
                let operations = operations.clone();
                let absolute_path = absolute_path.clone();
                move || operations.read_file(&absolute_path)
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;

            if signal.as_ref().map(|token| token.is_cancelled()).unwrap_or(false) {
                return Err("Operation aborted".to_string());
            }

            let raw_content = String::from_utf8_lossy(&buffer).into_owned();

            // Strip BOM before matching. The model will not include an invisible BOM in oldText.
            let super::edit_diff::StripBomResult { bom, text: content } = strip_bom(&raw_content);
            let original_ending = detect_line_ending(&content);
            let normalized_content = normalize_to_lf(&content);
            let AppliedEditsResult {
                base_content,
                new_content,
            } = apply_edits_to_normalized_content(&normalized_content, &edits, &path)?;

            let final_content = format!("{bom}{}", restore_line_endings(&new_content, original_ending));
            tokio::task::spawn_blocking({
                let operations = operations.clone();
                let absolute_path = absolute_path.clone();
                move || operations.write_file(&absolute_path, &final_content)
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;

            let (diff, first_changed_line) = generate_diff_string(&base_content, &new_content, 4, 1);
            Ok((
                format!("Successfully replaced {} block(s) in {}.", edits.len(), path),
                EditToolDetails {
                    diff,
                    first_changed_line,
                },
            ))
        }
    })
    .await;

    result
}

fn io_error_code(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => format!("E{code}"),
        None => format!("{:?}", error.kind()),
    }
}

/// Port of the edit tool's `renderCall`.
///
/// The TypeScript renderer kicks off `computeEditsDiff` asynchronously and
/// repaints when it resolves. Rust renders synchronously, so the diff is applied
/// through [`set_edit_preview`] before the component is built; callers that need
/// the async path call [`compute_edits_diff`] and then this function.
pub fn render_edit_call(
    state: &mut EditRenderState,
    last_component: Option<EditCallRenderComponent>,
    args: Option<&RenderableEditArgs>,
    theme: &dyn ToolTheme,
    expanded: bool,
    cwd: &str,
) -> EditCallRenderComponent {
    let mut component = get_edit_call_render_component(state, last_component);
    let preview_input = get_renderable_preview_input(args);
    let args_key = preview_args_key(preview_input.as_ref());

    if component.preview_args_key != args_key {
        component.preview = None;
        component.preview_args_key = args_key;
        component.preview_pending = false;
        component.settled_error = false;
    }

    build_edit_call_component(&mut component, args, theme, expanded, cwd);
    state.call_component = Some(component.clone());
    component
}

/// Async half of `renderCall`: resolve the predicted diff, then render.
pub async fn render_edit_call_with_preview(
    state: &mut EditRenderState,
    last_component: Option<EditCallRenderComponent>,
    args: Option<&RenderableEditArgs>,
    theme: &dyn ToolTheme,
    expanded: bool,
    cwd: &str,
) -> EditCallRenderComponent {
    let preview_input = get_renderable_preview_input(args);
    let args_key = preview_args_key(preview_input.as_ref());
    if let Some((path, edits)) = preview_input.as_ref() {
        let outcome = compute_edits_diff(path, edits, cwd).await;
        let mut component = get_edit_call_render_component(state, last_component);
        if component.preview_args_key != args_key {
            component.preview = None;
            component.preview_args_key = args_key.clone();
            component.preview_pending = false;
            component.settled_error = false;
        }
        set_edit_preview(&mut component, outcome.into(), args_key);
        state.call_component = Some(component.clone());
        build_edit_call_component(&mut component, args, theme, expanded, cwd);
        state.call_component = Some(component.clone());
        return component;
    }
    render_edit_call(state, last_component, args, theme, expanded, cwd)
}

/// Port of the edit tool's `renderResult`.
pub fn render_edit_result(
    state: &mut EditRenderState,
    result: &EditToolResultLike,
    args: Option<&RenderableEditArgs>,
    theme: &dyn ToolTheme,
    expanded: bool,
    cwd: &str,
    is_error: bool,
) -> Vec<String> {
    let preview_input = get_renderable_preview_input(args);
    let args_key = preview_args_key(preview_input.as_ref());
    let result_diff = if !is_error {
        result.details.as_ref().map(|details| details.diff.clone())
    } else {
        None
    };
    let mut changed = false;
    if let Some(call_component) = state.call_component.as_mut() {
        if let Some(result_diff) = result_diff {
            changed |= set_edit_preview(
                call_component,
                EditPreview::Result {
                    diff: result_diff,
                    first_changed_line: result.details.as_ref().and_then(|details| details.first_changed_line),
                },
                args_key,
            );
        }
        if call_component.settled_error != is_error {
            call_component.settled_error = is_error;
            changed = true;
        }
        if changed {
            let mut component = call_component.clone();
            build_edit_call_component(&mut component, args, theme, expanded, cwd);
            *call_component = component;
        }
    }

    let output = format_edit_result(args, state.call_component.as_ref().and_then(|c| c.preview.as_ref()), result, theme, is_error);
    let mut lines: Vec<String> = Vec::new();
    if let Some(output) = output {
        lines.push(String::new());
        lines.push(output);
    }
    lines
}

pub const EDIT_TOOL_DESCRIPTION: &str =
    "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.";

pub const EDIT_TOOL_PROMPT_SNIPPET: &str =
    "Make precise file edits with exact text replacement, including multiple disjoint edits in one call";

/// Port of `createEditToolDefinition`.
pub fn create_edit_tool_definition(cwd: &str, options: Option<&EditToolOptions>) -> ToolDefinition<EditToolDetails> {
    let operations = options.and_then(|options| options.operations.clone());
    let cwd = cwd.to_string();
    let execute: super::ToolExecuteFn<EditToolDetails> = Arc::new(
        move |_tool_call_id: String,
              params: Value,
              signal: Option<CancellationToken>,
              _on_update: Option<AgentToolUpdateCallback>,
              _ctx: ExtensionContext| {
            let cwd = cwd.clone();
            let operations = operations.clone();
            Box::pin(async move {
                let input: EditToolInput = serde_json::from_value(params)
                    .map_err(|error| anyhow::anyhow!("Edit tool input is invalid. {error}"))?;
                let (text, details) = execute_edit(&cwd, operations, &input, signal)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(AgentToolResult::new(
                    vec![ContentBlock::text(text)],
                    serde_json::to_value(details).unwrap_or(Value::Null),
                ))
            })
        },
    );

    ToolDefinition {
        name: "edit".to_string(),
        label: "edit".to_string(),
        description: EDIT_TOOL_DESCRIPTION.to_string(),
        prompt_snippet: Some(EDIT_TOOL_PROMPT_SNIPPET.to_string()),
        parameters: serde_json::from_str(EDIT_SCHEMA).expect("valid edit schema"),
        prepare_arguments: Some(Arc::new(prepare_edit_arguments)),
        render_shell: Some("self".to_string()),
        replay_built_in_tool_name: Some("edit".to_string()),
        execute,
        ..ToolDefinition::default()
    }
}

pub fn create_edit_tool(cwd: &str, options: Option<&EditToolOptions>) -> AgentTool {
    wrap_tool_definition(&create_edit_tool_definition(cwd, options), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tools::render_utils::{PlainTheme, RenderContentBlock};

    #[test]
    fn prepare_edit_arguments_parses_json_string_edits() {
        let input = serde_json::json!({
            "path": "a.txt",
            "edits": "[{\"oldText\":\"a\",\"newText\":\"b\"}]"
        });
        let prepared = prepare_edit_arguments(input);
        assert_eq!(prepared["edits"][0]["oldText"], serde_json::json!("a"));
    }

    #[test]
    fn prepare_edit_arguments_keeps_non_json_string_edits() {
        let input = serde_json::json!({ "path": "a.txt", "edits": "not json" });
        let prepared = prepare_edit_arguments(input);
        assert_eq!(prepared["edits"], serde_json::json!("not json"));
    }

    #[test]
    fn prepare_edit_arguments_merges_legacy_fields() {
        let input = serde_json::json!({
            "path": "a.txt",
            "oldText": "a",
            "newText": "b",
            "edits": [{ "oldText": "c", "newText": "d" }]
        });
        let prepared = prepare_edit_arguments(input);
        assert_eq!(prepared["edits"].as_array().expect("edits").len(), 2);
        assert_eq!(prepared["edits"][1]["oldText"], serde_json::json!("a"));
        assert!(prepared.get("oldText").is_none());
        assert!(prepared.get("newText").is_none());
    }

    #[test]
    fn validate_edit_input_rejects_empty_edits() {
        let input = EditToolInput {
            path: "a.txt".to_string(),
            edits: Vec::new(),
        };
        let error = validate_edit_input(&input).expect_err("must reject");
        assert_eq!(
            error,
            "Edit tool input is invalid. edits must contain at least one replacement."
        );
    }

    #[test]
    fn get_renderable_preview_input_prefers_edits_then_legacy_fields() {
        let args = RenderableEditArgs {
            file_path: Some("a.txt".to_string()),
            edits: Some(vec![Edit {
                old_text: "a".to_string(),
                new_text: "b".to_string(),
            }]),
            ..RenderableEditArgs::default()
        };
        let (path, edits) = get_renderable_preview_input(Some(&args)).expect("input");
        assert_eq!(path, "a.txt");
        assert_eq!(edits.len(), 1);

        let legacy = RenderableEditArgs {
            path: Some("b.txt".to_string()),
            old_text: Some("x".to_string()),
            new_text: Some("y".to_string()),
            ..RenderableEditArgs::default()
        };
        let (path, edits) = get_renderable_preview_input(Some(&legacy)).expect("input");
        assert_eq!(path, "b.txt");
        assert_eq!(edits[0].old_text, "x");

        assert!(get_renderable_preview_input(Some(&RenderableEditArgs::default())).is_none());
        assert!(get_renderable_preview_input(None).is_none());
    }

    #[test]
    fn format_edit_call_uses_path_and_invalid_marker() {
        let args = RenderableEditArgs {
            path: Some("/tmp/a.txt".to_string()),
            ..RenderableEditArgs::default()
        };
        assert_eq!(format_edit_call(Some(&args), &PlainTheme), "edit /tmp/a.txt");
        assert_eq!(format_edit_call(None, &PlainTheme), "edit [invalid arg]");
    }

    #[test]
    fn count_changed_lines_counts_diff_markers() {
        assert_eq!(count_changed_lines("-1 a\n+1 b\n 2 c"), (1, 1));
        assert_eq!(count_changed_lines("no markers"), (0, 0));
    }

    #[test]
    fn format_file_change_summary_line_truncates_to_width() {
        let line = format_file_change_summary_line("a.txt", "/tmp", (2, 3), true, 80);
        assert!(line.starts_with("\u{2570}\u{2500} "), "unexpected: {line}");
        assert!(line.contains("+2 -3"));
        let narrow = format_file_change_summary_line("a.txt", "/tmp", (2, 3), false, 6);
        assert_eq!(narrow.chars().count(), 6);
        assert!(narrow.ends_with("..."));
    }

    #[test]
    fn set_edit_preview_reports_changes() {
        let mut component = EditCallRenderComponent::new();
        let first = EditPreview::Result {
            diff: "-1 a\n+1 b".to_string(),
            first_changed_line: Some(1),
        };
        assert!(set_edit_preview(&mut component, first.clone(), Some("key".to_string())));
        assert!(!set_edit_preview(&mut component, first.clone(), Some("key".to_string())));
        let second = EditPreview::Result {
            diff: "-1 a\n+1 c".to_string(),
            first_changed_line: Some(1),
        };
        assert!(set_edit_preview(&mut component, second, Some("key".to_string())));
        let error = EditPreview::Error {
            error: "boom".to_string(),
        };
        assert!(set_edit_preview(&mut component, error.clone(), None));
        assert!(!set_edit_preview(&mut component, error, None));
        assert!(!component.preview_pending);
    }

    #[tokio::test]
    async fn execute_edit_replaces_text_and_reports_diff() {
        let dir = std::env::temp_dir().join(format!("pi-edit-tool-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("target.txt");
        std::fs::write(&file, "a\nb\nc\n").expect("write");
        let input = EditToolInput {
            path: "target.txt".to_string(),
            edits: vec![Edit {
                old_text: "b".to_string(),
                new_text: "B".to_string(),
            }],
        };
        let (text, details) = execute_edit(dir.to_string_lossy().as_ref(), None, &input, None)
            .await
            .expect("edited");
        let written = std::fs::read_to_string(&file).expect("read");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(text, "Successfully replaced 1 block(s) in target.txt.");
        assert_eq!(written, "a\nB\nc\n");
        assert_eq!(details.diff, "-2 b\n+2 B");
        assert_eq!(details.first_changed_line, Some(2));
    }

    #[tokio::test]
    async fn execute_edit_restores_crlf_and_bom() {
        let dir = std::env::temp_dir().join(format!("pi-edit-crlf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("crlf.txt");
        std::fs::write(&file, "\u{FEFF}a\r\nb\r\n").expect("write");
        let input = EditToolInput {
            path: "crlf.txt".to_string(),
            edits: vec![Edit {
                old_text: "b".to_string(),
                new_text: "B".to_string(),
            }],
        };
        execute_edit(dir.to_string_lossy().as_ref(), None, &input, None)
            .await
            .expect("edited");
        let written = std::fs::read_to_string(&file).expect("read");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(written, "\u{FEFF}a\r\nB\r\n");
    }

    #[tokio::test]
    async fn execute_edit_reports_missing_file_with_path_and_code() {
        let input = EditToolInput {
            path: "missing-edit-tool.txt".to_string(),
            edits: vec![Edit {
                old_text: "a".to_string(),
                new_text: "b".to_string(),
            }],
        };
        let error = execute_edit(
            std::env::temp_dir().to_string_lossy().as_ref(),
            None,
            &input,
            None,
        )
        .await
        .expect_err("must fail");
        assert!(error.starts_with("Could not edit file: missing-edit-tool.txt. Error code: E"), "{error}");
    }

    #[tokio::test]
    async fn execute_edit_rejects_an_already_cancelled_signal() {
        let token = CancellationToken::new();
        token.cancel();
        let input = EditToolInput {
            path: "a.txt".to_string(),
            edits: vec![Edit {
                old_text: "a".to_string(),
                new_text: "b".to_string(),
            }],
        };
        let error = execute_edit("/tmp", None, &input, Some(token))
            .await
            .expect_err("aborted");
        assert_eq!(error, "Operation aborted");
    }

    #[test]
    fn render_edit_result_reports_diff_and_error() {
        let mut state = EditRenderState {
            call_component: Some(EditCallRenderComponent::new()),
        };
        let result = EditToolResultLike {
            content: vec![RenderContentBlock::from_text("ok")],
            details: Some(EditToolDetails {
                diff: "-1 a\n+1 b".to_string(),
                first_changed_line: Some(1),
            }),
        };
        let args = RenderableEditArgs {
            path: Some("a.txt".to_string()),
            ..RenderableEditArgs::default()
        };
        let lines = render_edit_result(&mut state, &result, Some(&args), &PlainTheme, false, "/tmp", false);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("a.txt"));

        let error_result = EditToolResultLike {
            content: vec![RenderContentBlock::from_text("boom")],
            details: None,
        };
        let lines = render_edit_result(
            &mut state,
            &error_result,
            Some(&args),
            &PlainTheme,
            false,
            "/tmp",
            true,
        );
        assert_eq!(lines, vec![String::new(), "boom".to_string()]);
    }

    #[test]
    fn edit_tool_definition_metadata_matches_typescript() {
        let definition = create_edit_tool_definition("/tmp", None);
        assert_eq!(definition.name, "edit");
        assert_eq!(definition.label, "edit");
        assert_eq!(definition.description, EDIT_TOOL_DESCRIPTION);
        assert_eq!(definition.render_shell.as_deref(), Some("self"));
        assert_eq!(definition.replay_built_in_tool_name.as_deref(), Some("edit"));
        assert!(definition.prepare_arguments.is_some());
        assert_eq!(
            definition.parameters,
            serde_json::from_str::<Value>(EDIT_SCHEMA).expect("schema")
        );
    }
}
