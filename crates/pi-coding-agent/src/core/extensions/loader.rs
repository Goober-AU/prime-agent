//! Port of packages/coding-agent/src/core/extensions/loader.ts
//!
//! Extension loader. The TypeScript loader imports extension modules through
//! jiti; the Rust port has no JS module system, so a path is resolved and, when
//! it points at a Rust extension factory that was registered in-process, the
//! factory is invoked. Path discovery, alias resolution and error strings match
//! the TypeScript.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use super::types::{create_event_bus, EventBus};
use crate::core::source_info::{create_synthetic_source_info, SyntheticSourceInfoOptions};

use super::types::{
    AutocompleteItem, CancelledResult, Extension, ExtensionApi, ExtensionCommandContext, ExtensionContext,
    ExtensionContextActions, ExtensionEvent, ExtensionFactory, ExtensionFlag, ExtensionHandler, ExtensionRuntime,
    ExtensionRuntimeState, ExtensionShortcut, ExtensionUiContext, LoadExtensionError, LoadExtensionsResult,
    MessageRenderer, ProviderActions, ProviderConfig, RegisteredCommand, RegisteredTool, SharedExtension,
    ToolDefinition, EXTENSION_RUNTIME_STALE_MESSAGE,
};

/// `CONFIG_DIR_NAME` from config.ts.
pub const CONFIG_DIR_NAME: &str = ".prime/agent";

/// `[\u00A0\u2000-\u200A\u202F\u205F\u3000]`.
fn is_unicode_space(ch: char) -> bool {
    matches!(
        ch,
        '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

fn normalize_unicode_spaces(value: &str) -> String {
    value
        .chars()
        .map(|ch| if is_unicode_space(ch) { ' ' } else { ch })
        .collect()
}

fn expand_path(p: &str) -> String {
    let normalized = normalize_unicode_spaces(p);
    if let Some(rest) = normalized.strip_prefix("~/") {
        return join_path(&home_dir(), rest);
    }
    if let Some(rest) = normalized.strip_prefix('~') {
        return join_path(&home_dir(), rest);
    }
    normalized
}

fn home_dir() -> String {
    dirs::home_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn join_path(base: &str, part: &str) -> String {
    let base = base.trim_end_matches(['/', '\\']);
    format!("{base}{}{part}", std::path::MAIN_SEPARATOR)
}

/// `resolvePath(extPath, cwd)`.
pub fn resolve_path(ext_path: &str, cwd: &str) -> String {
    let expanded = expand_path(ext_path);
    if Path::new(&expanded).is_absolute() {
        return expanded;
    }
    crate::utils::paths::resolve_path(&join_path(cwd, &expanded))
}

/// In-process extension factory registry.
///
/// The TypeScript loader resolves a path to a JS module and imports it. Rust
/// extensions are compiled in, so the loader maps a resolved absolute path to a
/// registered factory; unregistered paths produce the TypeScript "does not
/// export a valid factory function" error.
fn factory_registry() -> &'static std::sync::Mutex<HashMap<String, ExtensionFactory>> {
    static REGISTRY: std::sync::OnceLock<std::sync::Mutex<HashMap<String, ExtensionFactory>>> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Register a compiled-in extension factory for an absolute path.
pub fn register_extension_factory(path: &str, factory: ExtensionFactory) {
    factory_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path.to_string(), factory);
}

fn load_extension_module(extension_path: &str) -> Option<ExtensionFactory> {
    factory_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(extension_path)
        .cloned()
}

/// `createExtensionRuntime()` - action methods throw until `bindCore()`.
pub fn create_extension_runtime() -> ExtensionRuntime {
    ExtensionRuntime::new(ExtensionRuntimeState::default())
}

/// The concrete `ExtensionAPI` handed to an extension factory.
///
/// Registration methods write to the shared extension object; action methods
/// delegate to the shared runtime, matching `createExtensionAPI`.
pub struct ExtensionApiImpl {
    extension: SharedExtension,
    runtime: ExtensionRuntime,
    cwd: String,
    event_bus: Arc<dyn EventBus>,
}

impl std::fmt::Debug for ExtensionApiImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionApiImpl")
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

impl ExtensionApiImpl {
    fn with_extension<T>(&self, f: impl FnOnce(&mut Extension) -> T) -> T {
        let mut guard = self
            .extension
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }

    fn assert_active(&self) -> Result<(), String> {
        self.runtime.assert_active()
    }
}

impl ExtensionApi for ExtensionApiImpl {
    fn on(&self, event_type: &str, handler: ExtensionHandler) {
        if self.assert_active().is_err() {
            return;
        }
        self.with_extension(|extension| {
            extension
                .handlers
                .entry(event_type.to_string())
                .or_default()
                .push(handler);
        });
    }

    fn register_tool(&self, tool: ToolDefinition) {
        if self.assert_active().is_err() {
            return;
        }
        let source_info = self.with_extension(|extension| {
            let name = tool.name.clone();
            let source_info = extension.source_info.clone();
            extension.tools.insert(
                name,
                RegisteredTool {
                    definition: tool,
                    source_info: source_info.clone(),
                },
            );
            source_info
        });
        let _ = source_info;
        self.runtime.refresh_tools();
    }

    fn register_command(&self, name: String, options: super::types::RegisterCommandOptions) {
        if self.assert_active().is_err() {
            return;
        }
        self.with_extension(|extension| {
            let source_info = extension.source_info.clone();
            extension.commands.insert(
                name.clone(),
                RegisteredCommand {
                    name,
                    source_info,
                    description: options.description,
                    get_argument_completions: options.get_argument_completions,
                    handler: options.handler.unwrap_or_else(|| {
                        Arc::new(|_, _| Box::pin(async { Ok(()) }))
                    }),
                },
            );
        });
    }

    fn register_shortcut(&self, shortcut: super::types::KeyId, options: super::types::RegisterShortcutOptions) {
        if self.assert_active().is_err() {
            return;
        }
        self.with_extension(|extension| {
            extension.shortcuts.insert(
                shortcut.clone(),
                ExtensionShortcut {
                    shortcut,
                    description: options.description,
                    handler: options.handler,
                    extension_path: extension.path.clone(),
                },
            );
        });
    }

    fn register_flag(&self, name: String, options: super::types::RegisterFlagOptions) {
        if self.assert_active().is_err() {
            return;
        }
        let default = options.default.clone();
        self.with_extension(|extension| {
            extension.flags.insert(
                name.clone(),
                ExtensionFlag {
                    name: name.clone(),
                    description: options.description,
                    flag_type: options.flag_type,
                    default: options.default,
                    extension_path: extension.path.clone(),
                },
            );
        });
        if let Some(default) = default {
            if !self.runtime.flag_values_has(&name) {
                self.runtime.flag_values_set(&name, default);
            }
        }
    }

    fn get_flag(&self, name: &str) -> Option<Value> {
        if self.assert_active().is_err() {
            return None;
        }
        let has_flag = self.with_extension(|extension| extension.flags.contains_key(name));
        if !has_flag {
            return None;
        }
        self.runtime.flag_values_get(name)
    }

    fn register_message_renderer(&self, custom_type: String, renderer: MessageRenderer) {
        if self.assert_active().is_err() {
            return;
        }
        self.with_extension(|extension| {
            extension.message_renderers.insert(custom_type, renderer);
        });
    }

    fn send_message(&self, message: super::types::CustomMessagePayload, options: Option<super::types::SendMessageOptions>) {
        if self.assert_active().is_err() {
            return;
        }
        let _ = self.runtime.send_message(message, options);
    }

    fn send_user_message(&self, content: Value, options: Option<super::types::SendUserMessageOptions>) {
        if self.assert_active().is_err() {
            return;
        }
        let _ = self.runtime.send_user_message(content, options);
    }

    fn append_entry(&self, custom_type: String, data: Option<Value>) {
        if self.assert_active().is_err() {
            return;
        }
        let _ = self.runtime.append_entry(&custom_type, data);
    }

    fn set_session_name(
        &self,
        name: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        if self.assert_active().is_err() {
            return Box::pin(async {});
        }
        match self.runtime.set_session_name(name) {
            Ok(future) => future,
            Err(_) => Box::pin(async {}),
        }
    }

    fn get_session_name(&self) -> Option<String> {
        if self.assert_active().is_err() {
            return None;
        }
        self.runtime.get_session_name().ok().flatten()
    }

    fn set_label(&self, entry_id: String, label: Option<String>) {
        if self.assert_active().is_err() {
            return;
        }
        let _ = self.runtime.set_label(entry_id, label);
    }

    fn exec(
        &self,
        command: String,
        args: Vec<String>,
        options: Option<super::types::ExecOptions>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<super::types::ExecResult, String>> + Send>> {
        if self.assert_active().is_err() {
            return Box::pin(async { Err(EXTENSION_RUNTIME_STALE_MESSAGE.to_string()) });
        }
        // Read the host-supplied env at call time so per-session vars (e.g.
        // herdr pane identity) are current, then let an explicit options.env win.
        let session_env = self.runtime.get_exec_env();
        let explicit_env = options.as_ref().and_then(|options| options.env.clone());
        let env = if session_env.is_some() || explicit_env.is_some() {
            let mut merged = session_env.unwrap_or_default();
            if let Some(explicit) = explicit_env {
                for (key, value) in explicit {
                    merged.insert(key, value);
                }
            }
            Some(merged)
        } else {
            None
        };
        let cwd = options
            .as_ref()
            .and_then(|options| options.cwd.clone())
            .unwrap_or_else(|| self.cwd.clone());
        let timeout = options.as_ref().and_then(|options| options.timeout);
        exec_command(command, args, cwd, env, timeout)
    }

    fn get_active_tools(&self) -> Vec<String> {
        if self.assert_active().is_err() {
            return Vec::new();
        }
        self.runtime.get_active_tools().unwrap_or_default()
    }

    fn get_all_tools(&self) -> Vec<super::types::ToolInfo> {
        if self.assert_active().is_err() {
            return Vec::new();
        }
        self.runtime.get_all_tools().unwrap_or_default()
    }

    fn set_active_tools(&self, tool_names: Vec<String>) {
        if self.assert_active().is_err() {
            return;
        }
        let _ = self.runtime.set_active_tools(tool_names);
    }

    fn get_commands(&self) -> Vec<crate::core::slash_commands::SlashCommandInfo> {
        if self.assert_active().is_err() {
            return Vec::new();
        }
        self.runtime.get_commands().unwrap_or_default()
    }

    fn set_model(
        &self,
        model: pi_ai::types::Model,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> {
        if self.assert_active().is_err() {
            return Box::pin(async { false });
        }
        let future = self.runtime.set_model(model);
        Box::pin(async move { future.await.unwrap_or(false) })
    }

    fn get_thinking_level(&self) -> pi_agent_core::types::ThinkingLevel {
        if self.assert_active().is_err() {
            return pi_agent_core::types::ThinkingLevel::Off;
        }
        self.runtime
            .get_thinking_level()
            .unwrap_or(pi_agent_core::types::ThinkingLevel::Off)
    }

    fn set_thinking_level(&self, level: pi_agent_core::types::ThinkingLevel) {
        if self.assert_active().is_err() {
            return;
        }
        let _ = self.runtime.set_thinking_level(level);
    }

    fn register_provider(&self, name: String, config: ProviderConfig) {
        if self.assert_active().is_err() {
            return;
        }
        let path = self.with_extension(|extension| extension.path.clone());
        self.runtime.register_provider(&name, config, Some(&path));
    }

    fn unregister_provider(&self, name: String) {
        if self.assert_active().is_err() {
            return;
        }
        let path = self.with_extension(|extension| extension.path.clone());
        self.runtime.unregister_provider(&name, Some(&path));
    }

    fn events(&self) -> Arc<dyn EventBus> {
        self.event_bus.clone()
    }
}

/// Private exec plumbing for `pi.exec()`.
///
/// blocked_on: needs core::exec::execCommand (the `ca-misc` slice owns it).
fn exec_command(
    command: String,
    args: Vec<String>,
    cwd: String,
    env: Option<serde_json::Map<String, Value>>,
    timeout_ms: Option<f64>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<super::types::ExecResult, String>> + Send>> {
    Box::pin(async move {
        let mut process = tokio::process::Command::new(&command);
        process.args(&args);
        if !cwd.is_empty() {
            process.current_dir(&cwd);
        }
        if let Some(env) = env {
            for (key, value) in env {
                match value {
                    Value::Null => {
                        process.env_remove(key);
                    }
                    Value::String(value) => {
                        process.env(key, value);
                    }
                    other => {
                        process.env(key, other.to_string());
                    }
                }
            }
        }
        process.kill_on_drop(true);

        let child = process.output();
        let output = match timeout_ms {
            Some(timeout) => {
                let duration = std::time::Duration::from_millis(timeout.max(0.0) as u64);
                match tokio::time::timeout(duration, child).await {
                    Ok(result) => result.map_err(|error| error.to_string())?,
                    Err(_) => {
                        return Ok(super::types::ExecResult {
                            stdout: String::new(),
                            stderr: String::new(),
                            code: 0.0,
                            killed: true,
                        })
                    }
                }
            }
            None => child.await.map_err(|error| error.to_string())?,
        };
        Ok(super::types::ExecResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            code: output.status.code().unwrap_or(0) as f64,
            killed: false,
        })
    })
}

/// `createExtension(extensionPath, resolvedPath)`.
fn create_extension(extension_path: &str, resolved_path: &str) -> Extension {
    let source = if extension_path.starts_with('<') && extension_path.ends_with('>') {
        let inner = &extension_path[1..extension_path.len() - 1];
        match inner.split(':').next() {
            Some(part) if !part.is_empty() => part.to_string(),
            _ => "temporary".to_string(),
        }
    } else {
        "local".to_string()
    };
    let base_dir = if extension_path.starts_with('<') {
        None
    } else {
        parent_dir(resolved_path)
    };

    Extension {
        path: extension_path.to_string(),
        resolved_path: resolved_path.to_string(),
        source_info: create_synthetic_source_info(
            extension_path,
            &SyntheticSourceInfoOptions {
                source,
                scope: None,
                origin: None,
                base_dir,
            },
        ),
        handlers: HashMap::new(),
        tools: HashMap::new(),
        message_renderers: HashMap::new(),
        commands: HashMap::new(),
        flags: HashMap::new(),
        shortcuts: HashMap::new(),
    }
}

fn parent_dir(value: &str) -> Option<String> {
    Path::new(value)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.to_string_lossy().to_string())
}

/// `createExtensionAPI(extension, runtime, cwd, eventBus)`.
fn create_extension_api(
    extension: SharedExtension,
    runtime: ExtensionRuntime,
    cwd: String,
    event_bus: Arc<dyn EventBus>,
) -> Arc<dyn ExtensionApi> {
    Arc::new(ExtensionApiImpl {
        extension,
        runtime,
        cwd,
        event_bus,
    })
}

async fn load_extension(
    extension_path: &str,
    cwd: &str,
    event_bus: Arc<dyn EventBus>,
    runtime: ExtensionRuntime,
) -> (Option<SharedExtension>, Option<String>) {
    let resolved_path = resolve_path(extension_path, cwd);

    match load_extension_module(&resolved_path) {
        None => (
            None,
            Some(format!(
                "Extension does not export a valid factory function: {extension_path}"
            )),
        ),
        Some(factory) => {
            let extension = Arc::new(std::sync::Mutex::new(create_extension(extension_path, &resolved_path)));
            let api = create_extension_api(extension.clone(), runtime, cwd.to_string(), event_bus);
            match factory(api).await {
                Ok(()) => (Some(extension), None),
                Err(error) => (None, Some(format!("Failed to load extension: {error}"))),
            }
        }
    }
}

/// Create an `Extension` from an inline factory function.
pub async fn load_extension_from_factory(
    factory: ExtensionFactory,
    cwd: &str,
    event_bus: Arc<dyn EventBus>,
    runtime: ExtensionRuntime,
    extension_path: Option<&str>,
) -> Result<SharedExtension, String> {
    let extension_path = extension_path.unwrap_or("<inline>");
    let extension = Arc::new(std::sync::Mutex::new(create_extension(extension_path, extension_path)));
    let api = create_extension_api(extension.clone(), runtime, cwd.to_string(), event_bus);
    factory(api).await?;
    Ok(extension)
}

/// Load extensions from paths.
pub async fn load_extensions(
    paths: &[String],
    cwd: &str,
    event_bus: Option<Arc<dyn EventBus>>,
) -> LoadExtensionsResult {
    let mut extensions: Vec<SharedExtension> = Vec::new();
    let mut errors: Vec<LoadExtensionError> = Vec::new();
    let resolved_event_bus = event_bus.unwrap_or_else(create_event_bus);
    let runtime = create_extension_runtime();

    for ext_path in paths {
        let (extension, error) = load_extension(ext_path, cwd, resolved_event_bus.clone(), runtime.clone()).await;

        if let Some(error) = error {
            errors.push(LoadExtensionError {
                path: ext_path.clone(),
                error,
            });
            continue;
        }

        if let Some(extension) = extension {
            extensions.push(extension);
        }
    }

    LoadExtensionsResult {
        extensions,
        errors,
        runtime,
    }
}

/// `PiManifest` from a `package.json` `pi` field.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PiManifest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub themes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<String>>,
}

fn read_pi_manifest(package_json_path: &str) -> Option<PiManifest> {
    let content = std::fs::read_to_string(package_json_path).ok()?;
    let pkg: Value = serde_json::from_str(&content).ok()?;
    let pi = pkg.get("pi")?;
    if !pi.is_object() {
        return None;
    }
    serde_json::from_value(pi.clone()).ok()
}

fn is_extension_file(name: &str) -> bool {
    name.ends_with(".ts") || name.ends_with(".js")
}

/// Resolve extension entry points from a directory.
///
/// Checks for:
/// 1. `package.json` with a `pi.extensions` field -> returns declared paths
/// 2. `index.ts` or `index.js` -> returns the index file
pub fn resolve_extension_entries(dir: &str) -> Option<Vec<String>> {
    let package_json_path = join_path(dir, "package.json");
    if Path::new(&package_json_path).exists() {
        if let Some(manifest) = read_pi_manifest(&package_json_path) {
            if let Some(declared) = manifest.extensions.as_ref().filter(|list| !list.is_empty()) {
                let mut entries: Vec<String> = Vec::new();
                for ext_path in declared {
                    let resolved_ext_path = crate::utils::paths::resolve_path(&join_path(dir, ext_path));
                    if Path::new(&resolved_ext_path).exists() {
                        entries.push(resolved_ext_path);
                    }
                }
                if !entries.is_empty() {
                    return Some(entries);
                }
            }
        }
    }

    let index_ts = join_path(dir, "index.ts");
    let index_js = join_path(dir, "index.js");
    if Path::new(&index_ts).exists() {
        return Some(vec![index_ts]);
    }
    if Path::new(&index_js).exists() {
        return Some(vec![index_js]);
    }

    None
}

/// Discover extensions in a directory.
///
/// Discovery rules:
/// 1. Direct files: `extensions/*.ts` or `*.js` -> load
/// 2. Subdirectory with index: `extensions/*/index.ts` or `index.js` -> load
/// 3. Subdirectory with package.json: `extensions/*/package.json` with a `pi`
///    field -> load what it declares
///
/// No recursion beyond one level. Complex packages must use a package.json manifest.
pub fn discover_extensions_in_dir(dir: &str) -> Vec<String> {
    if !Path::new(dir).exists() {
        return Vec::new();
    }

    let mut discovered: Vec<String> = Vec::new();

    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    // `readdirSync` order is filesystem order and is observable in the loaded
    // order, so no sort is introduced here.
    let mut collected: Vec<(String, bool, bool)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let file_type = entry.file_type().ok();
        let is_symlink = file_type.map(|kind| kind.is_symlink()).unwrap_or(false);
        let is_file = file_type.map(|kind| kind.is_file()).unwrap_or(false);
        collected.push((name, is_file, is_symlink));
    }

    for (name, is_file, is_symlink) in collected {
        let entry_path = join_path(dir, &name);

        if (is_file || is_symlink) && is_extension_file(&name) {
            discovered.push(entry_path);
            continue;
        }

        if is_file && !is_symlink {
            continue;
        }

        if is_symlink {
            let resolved = std::fs::metadata(&entry_path)
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false);
            if !resolved {
                continue;
            }
        }

        if let Some(entries) = resolve_extension_entries(&entry_path) {
            discovered.extend(entries);
        }
    }

    discovered
}

/// Discover and load extensions from standard locations.
pub async fn discover_and_load_extensions(
    configured_paths: &[String],
    cwd: &str,
    agent_dir: Option<&str>,
    event_bus: Option<Arc<dyn EventBus>>,
) -> LoadExtensionsResult {
    let agent_dir = agent_dir
        .map(str::to_string)
        .unwrap_or_else(crate::config::get_agent_dir);
    let mut all_paths: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut add_paths = |paths: Vec<String>, all_paths: &mut Vec<String>, seen: &mut HashSet<String>| {
        for p in paths {
            let resolved = crate::utils::paths::resolve_path(&p);
            if seen.insert(resolved) {
                all_paths.push(p);
            }
        }
    };

    let local_ext_dir = join_path(cwd, CONFIG_DIR_NAME);
    let local_ext_dir = join_path(&local_ext_dir, "extensions");
    add_paths(
        discover_extensions_in_dir(&local_ext_dir),
        &mut all_paths,
        &mut seen,
    );

    let global_ext_dir = join_path(&agent_dir, "extensions");
    add_paths(
        discover_extensions_in_dir(&global_ext_dir),
        &mut all_paths,
        &mut seen,
    );

    for p in configured_paths {
        let resolved = resolve_path(p, cwd);
        let is_dir = std::fs::metadata(&resolved)
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false);
        if Path::new(&resolved).exists() && is_dir {
            if let Some(entries) = resolve_extension_entries(&resolved) {
                add_paths(entries, &mut all_paths, &mut seen);
                continue;
            }
            add_paths(
                discover_extensions_in_dir(&resolved),
                &mut all_paths,
                &mut seen,
            );
            continue;
        }

        add_paths(vec![resolved], &mut all_paths, &mut seen);
    }

    load_extensions(&all_paths, cwd, event_bus).await
}

/// Helper used by the resource loader to bind provider actions on the runtime.
pub fn bind_provider_actions(runtime: &ExtensionRuntime, actions: ProviderActions) {
    let mut guard = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.provider_actions = Some(actions);
}

/// Helper used by the runner to install the action implementations.
pub fn bind_actions(runtime: &ExtensionRuntime, actions: super::types::ExtensionActions) {
    let mut guard = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.actions = Some(actions);
}

/// Unused import guards for the module surface kept for parity.
#[allow(dead_code)]
fn _surface(
    _: Option<AutocompleteItem>,
    _: Option<CancelledResult>,
    _: Option<ExtensionCommandContextActionsMarker>,
    _: Option<ExtensionContextActions>,
    _: Option<ExtensionEvent>,
    _: Option<std::sync::Arc<ExtensionUIContextMarker>>,
    _: Option<PathBuf>,
    _: Option<Value>,
) {
}

#[allow(dead_code)]
type ExtensionCommandContextActionsMarker = super::types::ExtensionCommandContextActions;
#[allow(dead_code)]
type ExtensionUIContextMarker = dyn ExtensionUiContext;
#[allow(dead_code)]
type ExtensionContextMarker = dyn ExtensionContext;
#[allow(dead_code)]
type ExtensionCommandContextMarker = dyn ExtensionCommandContext;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extensions::types::{
        CustomMessagePayload, ExtensionHandler, RegisterCommandOptions, RegisterFlagOptions, SendMessageOptions,
    };
    
    fn runtime_with_actions(runtime: &ExtensionRuntime) {
        let actions = crate::core::extensions::types::ExtensionActions {
            send_message: Arc::new(|_: CustomMessagePayload, _: Option<SendMessageOptions>| {}),
            send_user_message: Arc::new(|_: Value, _| {}),
            append_entry: Arc::new(|_: String, _: Option<Value>| {}),
            set_session_name: Arc::new(|_: String| Box::pin(async {})),
            get_session_name: Arc::new(|| Some("session".to_string())),
            set_label: Arc::new(|_: String, _: Option<String>| {}),
            get_active_tools: Arc::new(|| vec!["ipython".to_string()]),
            get_all_tools: Arc::new(Vec::new),
            set_active_tools: Arc::new(|_: Vec<String>| {}),
            refresh_tools: Arc::new(|| {}),
            get_commands: Arc::new(Vec::new),
            set_model: Arc::new(|_: pi_ai::types::Model| Box::pin(async { true })),
            get_thinking_level: Arc::new(|| pi_agent_core::types::ThinkingLevel::Off),
            set_thinking_level: Arc::new(|_: pi_agent_core::types::ThinkingLevel| {}),
        };
        bind_actions(runtime, actions);
    }

    #[test]
    fn expand_path_handles_tilde_and_unicode_spaces() {
        let home = home_dir();
        assert_eq!(expand_path("~/x"), join_path(&home, "x"));
        assert_eq!(expand_path("~x"), join_path(&home, "x"));
        assert_eq!(expand_path("plain"), "plain");
        assert_eq!(expand_path("a\u{00A0}b"), "a b");
        assert_eq!(expand_path("a\u{3000}b"), "a b");
    }

    #[test]
    fn resolve_path_uses_cwd_for_relative_paths() {
        let cwd = if cfg!(windows) { "C:\\work" } else { "/work" };
        let resolved = resolve_path("sub/ext.ts", cwd);
        assert!(resolved.ends_with("ext.ts"));
        let absolute = if cfg!(windows) { "C:\\abs\\ext.ts" } else { "/abs/ext.ts" };
        assert_eq!(resolve_path(absolute, cwd), absolute);
    }

    #[test]
    fn create_extension_derives_source_and_base_dir() {
        let local = create_extension("/tmp/ext.ts", "/tmp/ext.ts");
        assert_eq!(local.source_info.source, "local");
        assert_eq!(local.source_info.scope, "temporary");
        assert_eq!(local.source_info.origin, "top-level");
        assert_eq!(local.source_info.base_dir.as_deref(), parent_dir("/tmp/ext.ts").as_deref());

        let inline = create_extension("<inline:2>", "<inline:2>");
        assert_eq!(inline.source_info.source, "inline");
        assert_eq!(inline.source_info.base_dir, None);

        let bare = create_extension("<>", "<>");
        assert_eq!(bare.source_info.source, "temporary");
    }

    #[test]
    fn extension_api_registration_writes_to_the_extension() {
        let runtime = create_extension_runtime();
        runtime_with_actions(&runtime);
        let extension = Arc::new(std::sync::Mutex::new(create_extension("<inline>", "<inline>")));
        let api = create_extension_api(extension.clone(), runtime, "/cwd".to_string(), create_event_bus());

        let handler: ExtensionHandler = Arc::new(|_, _| Box::pin(async { None }));
        api.on("session_start", handler);
        api.register_flag(
            "my-flag".to_string(),
            RegisterFlagOptions {
                description: None,
                flag_type: "boolean".to_string(),
                default: Some(Value::Bool(true)),
            },
        );
        api.register_command(
            "my-command".to_string(),
            RegisterCommandOptions {
                description: Some("desc".to_string()),
                get_argument_completions: None,
                handler: None,
            },
        );

        let guard = extension.lock().unwrap();
        assert_eq!(guard.handlers.get("session_start").unwrap().len(), 1);
        assert!(guard.flags.contains_key("my-flag"));
        assert_eq!(guard.commands.get("my-command").unwrap().description.as_deref(), Some("desc"));
        assert_eq!(guard.commands.get("my-command").unwrap().source_info.path, "<inline>");
    }

    #[test]
    fn flag_defaults_are_registered_once() {
        let runtime = create_extension_runtime();
        runtime_with_actions(&runtime);
        let extension = Arc::new(std::sync::Mutex::new(create_extension("<inline>", "<inline>")));
        let api = create_extension_api(extension, runtime.clone(), "/cwd".to_string(), create_event_bus());

        api.register_flag(
            "flag".to_string(),
            RegisterFlagOptions {
                description: None,
                flag_type: "string".to_string(),
                default: Some(Value::String("first".to_string())),
            },
        );
        runtime.flag_values_set("flag", Value::String("cli".to_string()));
        api.register_flag(
            "flag".to_string(),
            RegisterFlagOptions {
                description: None,
                flag_type: "string".to_string(),
                default: Some(Value::String("second".to_string())),
            },
        );

        assert_eq!(api.get_flag("flag"), Some(Value::String("cli".to_string())));
        assert_eq!(api.get_flag("missing"), None);
    }

    #[test]
    fn stale_extension_instances_stop_registering() {
        let runtime = create_extension_runtime();
        runtime_with_actions(&runtime);
        let extension = Arc::new(std::sync::Mutex::new(create_extension("<inline>", "<inline>")));
        let api = create_extension_api(extension.clone(), runtime.clone(), "/cwd".to_string(), create_event_bus());

        runtime.invalidate(None);
        let handler: ExtensionHandler = Arc::new(|_, _| Box::pin(async { None }));
        api.on("session_start", handler);
        api.register_flag(
            "later".to_string(),
            RegisterFlagOptions {
                description: None,
                flag_type: "boolean".to_string(),
                default: None,
            },
        );

        let guard = extension.lock().unwrap();
        assert!(guard.handlers.is_empty());
        assert!(guard.flags.is_empty());
        assert!(runtime.assert_active().is_err());
    }

    #[test]
    fn provider_registrations_queue_before_bind_core() {
        let runtime = create_extension_runtime();
        runtime_with_actions(&runtime);
        let extension = Arc::new(std::sync::Mutex::new(create_extension("<inline>", "<inline>")));
        let api = create_extension_api(extension, runtime.clone(), "/cwd".to_string(), create_event_bus());

        api.register_provider("my-proxy".to_string(), super::super::types::ProviderConfig::default());
        let queued = runtime.take_pending_provider_registrations();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].name, "my-proxy");
        assert_eq!(queued[0].extension_path, "<inline>");
        assert!(runtime.take_pending_provider_registrations().is_empty());
    }

    #[tokio::test]
    async fn loading_an_unregistered_path_reports_the_typescript_error() {
        let result = load_extensions(&["/definitely/missing/ext.ts".to_string()], "/cwd", None).await;
        assert!(result.extensions.is_empty());
        assert_eq!(
            result.errors[0].error,
            "Extension does not export a valid factory function: /definitely/missing/ext.ts"
        );
    }

    #[tokio::test]
    async fn inline_factories_are_loaded_and_failures_are_wrapped() {
        let runtime = create_extension_runtime();
        let factory: ExtensionFactory = Arc::new(|_api| Box::pin(async { Ok(()) }));
        let extension = load_extension_from_factory(factory, "/cwd", create_event_bus(), runtime.clone(), None)
            .await
            .unwrap();
        assert_eq!(extension.lock().unwrap().path, "<inline>");

        let failing: ExtensionFactory = Arc::new(|_api| Box::pin(async { Err("boom".to_string()) }));
        let error = load_extension_from_factory(failing, "/cwd", create_event_bus(), runtime, Some("<inline:7>"))
            .await
            .unwrap_err();
        assert_eq!(error, "boom");
    }

    #[test]
    fn discovery_returns_direct_files_and_index_entries() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        std::fs::write(dir.path().join("direct.ts"), "").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("index.ts"), "").unwrap();
        let packaged = dir.path().join("packaged");
        std::fs::create_dir_all(&packaged).unwrap();
        std::fs::write(
            packaged.join("package.json"),
            r#"{"pi":{"extensions":["main.ts","missing.ts"]}}"#,
        )
        .unwrap();
        std::fs::write(packaged.join("main.ts"), "").unwrap();

        let mut discovered = discover_extensions_in_dir(&root);
        discovered.sort();
        assert_eq!(discovered.len(), 3);
        assert!(discovered.iter().any(|path| path.ends_with("direct.ts")));
        assert!(discovered.iter().any(|path| path.ends_with("nested\\index.ts")
            || path.ends_with("nested/index.ts")));
        assert!(discovered.iter().any(|path| path.ends_with("packaged\\main.ts")
            || path.ends_with("packaged/main.ts")));
        assert!(!discovered.iter().any(|path| path.ends_with("notes.txt")));
        assert!(!discovered.iter().any(|path| path.ends_with("missing.ts")));
    }

    #[test]
    fn resolve_extension_entries_prefers_the_manifest_then_index() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        assert_eq!(resolve_extension_entries(&root), None);
        std::fs::write(dir.path().join("index.js"), "").unwrap();
        let entries = resolve_extension_entries(&root).unwrap();
        assert!(entries[0].ends_with("index.js"));
    }

    #[test]
    fn discovery_of_a_missing_directory_is_empty() {
        assert!(discover_extensions_in_dir("/definitely/missing/extensions").is_empty());
    }
}
