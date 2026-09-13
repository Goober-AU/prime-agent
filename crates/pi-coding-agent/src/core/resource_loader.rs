//! Port of packages/coding-agent/src/core/resource-loader.ts
//!
//! Loads extensions, skills, prompts, themes and context files from user,
//! project, package and CLI sources, applying settings overrides in the same
//! order as the TypeScript.
//!
//! Rust notes:
//!   - the TypeScript object is a single mutable identity shared by callers, so
//!     `DefaultResourceLoader` keeps its mutable state behind a `Mutex` and the
//!     trait takes `&self`.
//!   - `Theme` is not `Clone`, so loaded themes are held as `Arc<Theme>`; an
//!     override closure receives and returns the owned list.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use futures::FutureExt;

use crate::config::get_bundled_skills_dir;
// `export type { ResourceCollision, ResourceDiagnostic } from "./diagnostics.js";`
pub use crate::core::diagnostics::{ResourceCollision, ResourceDiagnostic};
use crate::core::diagnostics::{
    RESOURCE_DIAGNOSTIC_COLLISION, RESOURCE_DIAGNOSTIC_ERROR, RESOURCE_DIAGNOSTIC_WARNING,
};
use crate::core::event_bus::{create_event_bus, EventBus};
use crate::core::extensions::loader::{create_extension_runtime, load_extension_from_factory, load_extensions};
use crate::core::extensions::types::{
    ExtensionFactory, ExtensionRuntime, LoadExtensionError, LoadExtensionsResult, SharedExtension,
};
use crate::core::package_manager::{DefaultPackageManager, PackageManagerOptions, PathMetadata, ResolvedPaths};
use crate::core::prompt_templates::{load_prompt_templates, LoadPromptTemplatesOptions, PromptTemplate};
use crate::core::settings_manager::SettingsManager;
use crate::core::skills::{load_skills, LoadSkillsOptions, Skill};
use crate::core::source_info::{
    SourceInfo, SOURCE_ORIGIN_PACKAGE, SOURCE_ORIGIN_TOP_LEVEL, SOURCE_SCOPE_PROJECT, SOURCE_SCOPE_TEMPORARY,
    SOURCE_SCOPE_USER,
};
use crate::core::system_prompt::ContextFile;
use crate::modes::interactive::theme::theme::{load_theme_from_path, Theme};
use crate::utils::paths::{canonicalize_path, is_local_path};


/// `CONFIG_DIR_NAME` from config.ts (package.json `piConfig.configDir`).
pub const CONFIG_DIR_NAME: &str = ".prime/agent";

/// `{ path; metadata }` resource entry used by `extendResources`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourcePathEntry {
    pub path: String,
    pub metadata: PathMetadata,
}

/// `ResourceExtensionPaths`.
#[derive(Debug, Clone, Default)]
pub struct ResourceExtensionPaths {
    pub skill_paths: Option<Vec<ResourcePathEntry>>,
    pub prompt_paths: Option<Vec<ResourcePathEntry>>,
    pub theme_paths: Option<Vec<ResourcePathEntry>>,
}

/// `{ skills; diagnostics }`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SkillsResult {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// `{ prompts; diagnostics }`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PromptsResult {
    pub prompts: Vec<PromptTemplate>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// `{ themes; diagnostics }`. Themes are `Arc`-shared because `Theme` is not `Clone`.
#[derive(Debug, Clone, Default)]
pub struct ThemesResult {
    pub themes: Vec<Arc<Theme>>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// Owned theme list handed to `themesOverride` (still mutable at that point).
#[derive(Debug, Default)]
pub struct OwnedThemesResult {
    pub themes: Vec<Theme>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// `{ agentsFiles }`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentsFilesResult {
    pub agents_files: Vec<ContextFile>,
}

/// `interface ResourceLoader`.
pub trait ResourceLoader: Send + Sync {
    fn get_extensions(&self) -> LoadExtensionsResult;
    fn get_skills(&self) -> SkillsResult;
    fn get_prompts(&self) -> PromptsResult;
    fn get_themes(&self) -> ThemesResult;
    fn get_agents_files(&self) -> AgentsFilesResult;
    fn get_system_prompt(&self) -> Option<String>;
    fn get_append_system_prompt(&self) -> Vec<String>;
    fn extend_resources(&self, paths: ResourceExtensionPaths);
    fn reload(&self) -> BoxFuture<'_, ()>;
}

/// `resolvePromptInput(input, description)`.
fn resolve_prompt_input(input: Option<String>, description: &str) -> Option<String> {
    let input = input?;
    if Path::new(&input).exists() {
        match std::fs::read_to_string(&input) {
            Ok(content) => return Some(content),
            Err(error) => {
                // `console.error(chalk.yellow(...))`.
                eprintln!("Warning: Could not read {description} file {input}: {error}");
                return Some(input);
            }
        }
    }
    Some(input)
}

/// `loadContextFileFromDir(dir)`.
fn load_context_file_from_dir(dir: &str) -> Option<ContextFile> {
    const CANDIDATES: [&str; 4] = ["AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];
    for filename in CANDIDATES {
        let file_path = join_path(dir, filename);
        if Path::new(&file_path).exists() {
            match std::fs::read_to_string(&file_path) {
                Ok(content) => {
                    return Some(ContextFile {
                        path: file_path,
                        content,
                    })
                }
                Err(error) => {
                    eprintln!("Warning: Could not read {file_path}: {error}");
                }
            }
        }
    }
    None
}

/// `loadProjectContextFiles({ cwd, agentDir })`.
pub fn load_project_context_files(cwd: &str, agent_dir: &str) -> Vec<ContextFile> {
    let resolved_cwd = cwd.to_string();
    let resolved_agent_dir = agent_dir.to_string();

    let mut context_files: Vec<ContextFile> = Vec::new();
    let mut seen_paths: Vec<String> = Vec::new();

    if let Some(global_context) = load_context_file_from_dir(&resolved_agent_dir) {
        seen_paths.push(global_context.path.clone());
        context_files.push(global_context);
    }

    let mut ancestor_context_files: Vec<ContextFile> = Vec::new();

    let mut current_dir = resolve_absolute(&resolved_cwd);
    loop {
        if let Some(context_file) = load_context_file_from_dir(&current_dir) {
            if !seen_paths.contains(&context_file.path) {
                ancestor_context_files.insert(0, context_file.clone());
                seen_paths.push(context_file.path);
            }
        }

        let parent_dir = resolve_absolute(&join_path(&current_dir, ".."));
        if parent_dir == current_dir {
            break;
        }
        current_dir = parent_dir;
    }

    context_files.append(&mut ancestor_context_files);
    context_files
}

/// `DefaultResourceLoaderOptions`.
pub struct DefaultResourceLoaderOptions {
    pub cwd: String,
    pub agent_dir: String,
    pub settings_manager: Option<Arc<Mutex<SettingsManager>>>,
    pub event_bus: Option<Arc<dyn EventBus>>,
    pub additional_extension_paths: Vec<String>,
    pub additional_skill_paths: Vec<String>,
    pub additional_prompt_template_paths: Vec<String>,
    pub additional_theme_paths: Vec<String>,
    pub extension_factories: Vec<ExtensionFactory>,
    pub no_extensions: bool,
    pub no_skills: bool,
    pub no_prompt_templates: bool,
    pub no_themes: bool,
    pub no_context_files: bool,
    /// `Some(None)` = disabled (`null` in TypeScript); `None` = bundled default.
    pub bundled_skills_dir: Option<Option<String>>,
    pub extra_builtin_skill_overrides: Option<Arc<dyn Fn() -> Vec<String> + Send + Sync>>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<Vec<String>>,
    pub extensions_override: Option<Arc<dyn Fn(LoadExtensionsResult) -> LoadExtensionsResult + Send + Sync>>,
    pub skills_override: Option<Arc<dyn Fn(SkillsResult) -> SkillsResult + Send + Sync>>,
    pub prompts_override: Option<Arc<dyn Fn(PromptsResult) -> PromptsResult + Send + Sync>>,
    pub themes_override: Option<Arc<dyn Fn(OwnedThemesResult) -> OwnedThemesResult + Send + Sync>>,
    pub agents_files_override: Option<Arc<dyn Fn(AgentsFilesResult) -> AgentsFilesResult + Send + Sync>>,
    pub system_prompt_override: Option<Arc<dyn Fn(Option<String>) -> Option<String> + Send + Sync>>,
    pub append_system_prompt_override: Option<Arc<dyn Fn(Vec<String>) -> Vec<String> + Send + Sync>>,
}

/// `DefaultResourceLoaderOptions` with empty `cwd`/`agentDir`.
///
/// The TypeScript declares both as required; `Default` exists so callers that
/// take the options from another options object (`{ ...resourceLoaderOptions }`
/// with no loader options supplied) can build the same "no extra options" value.
impl Default for DefaultResourceLoaderOptions {
    fn default() -> Self {
        Self::new("", "")
    }
}

impl DefaultResourceLoaderOptions {
    /// `{ cwd, agentDir }` - every other option defaults exactly like the TypeScript.
    pub fn new(cwd: &str, agent_dir: &str) -> Self {
        Self {
            cwd: cwd.to_string(),
            agent_dir: agent_dir.to_string(),
            settings_manager: None,
            event_bus: None,
            additional_extension_paths: Vec::new(),
            additional_skill_paths: Vec::new(),
            additional_prompt_template_paths: Vec::new(),
            additional_theme_paths: Vec::new(),
            extension_factories: Vec::new(),
            no_extensions: false,
            no_skills: false,
            no_prompt_templates: false,
            no_themes: false,
            no_context_files: false,
            bundled_skills_dir: None,
            extra_builtin_skill_overrides: None,
            system_prompt: None,
            append_system_prompt: None,
            extensions_override: None,
            skills_override: None,
            prompts_override: None,
            themes_override: None,
            agents_files_override: None,
            system_prompt_override: None,
            append_system_prompt_override: None,
        }
    }
}

/// `loadExtensionsResult` + skills/prompts/themes/diagnostics state.
#[derive(Default)]
struct LoaderState {
    extensions: Vec<SharedExtension>,
    extension_errors: Vec<LoadExtensionError>,
    runtime: Option<ExtensionRuntime>,
    loaded_extension_paths: Vec<String>,
    skills: Vec<Skill>,
    skill_diagnostics: Vec<ResourceDiagnostic>,
    prompts: Vec<PromptTemplate>,
    prompt_diagnostics: Vec<ResourceDiagnostic>,
    themes: Vec<Arc<Theme>>,
    theme_diagnostics: Vec<ResourceDiagnostic>,
    agents_files: Vec<ContextFile>,
    system_prompt: Option<String>,
    append_system_prompt: Vec<String>,
    last_skill_paths: Vec<String>,
    last_prompt_paths: Vec<String>,
    last_theme_paths: Vec<String>,
}

/// The mutable state behind [`DefaultResourceLoader`].
///
/// TypeScript's `DefaultResourceLoader` is one object that `reload()` mutates;
/// Rust shares it through an `Arc` so cloned handles reload the same state.
pub struct LoaderInner {
    cwd: String,
    agent_dir: String,
    settings_manager: Arc<Mutex<SettingsManager>>,
    event_bus: Arc<dyn EventBus>,
    package_manager: DefaultPackageManager,
    bundled_skills_dir: Option<String>,
    additional_extension_paths: Vec<String>,
    additional_skill_paths: Vec<String>,
    additional_prompt_template_paths: Vec<String>,
    additional_theme_paths: Vec<String>,
    extension_factories: Vec<ExtensionFactory>,
    no_extensions: bool,
    no_skills: bool,
    no_prompt_templates: bool,
    no_themes: bool,
    no_context_files: bool,
    system_prompt_source: Option<String>,
    append_system_prompt_source: Option<Vec<String>>,
    extensions_override: Option<Arc<dyn Fn(LoadExtensionsResult) -> LoadExtensionsResult + Send + Sync>>,
    skills_override: Option<Arc<dyn Fn(SkillsResult) -> SkillsResult + Send + Sync>>,
    prompts_override: Option<Arc<dyn Fn(PromptsResult) -> PromptsResult + Send + Sync>>,
    themes_override: Option<Arc<dyn Fn(OwnedThemesResult) -> OwnedThemesResult + Send + Sync>>,
    agents_files_override: Option<Arc<dyn Fn(AgentsFilesResult) -> AgentsFilesResult + Send + Sync>>,
    system_prompt_override: Option<Arc<dyn Fn(Option<String>) -> Option<String> + Send + Sync>>,
    append_system_prompt_override: Option<Arc<dyn Fn(Vec<String>) -> Vec<String> + Send + Sync>>,
    extension_skill_source_infos: Mutex<HashMap<String, SourceInfo>>,
    extension_prompt_source_infos: Mutex<HashMap<String, SourceInfo>>,
    extension_theme_source_infos: Mutex<HashMap<String, SourceInfo>>,
    state: Mutex<LoaderState>,
}

impl std::fmt::Debug for LoaderInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultResourceLoader")
            .field("cwd", &self.cwd)
            .field("agent_dir", &self.agent_dir)
            .finish_non_exhaustive()
    }
}

impl LoaderInner {
    pub fn new(options: DefaultResourceLoaderOptions) -> Self {
        let settings_manager = options
            .settings_manager
            .unwrap_or_else(|| Arc::new(Mutex::new(SettingsManager::create(&options.cwd, Some(&options.agent_dir)))));
        let bundled_skills_dir = match options.bundled_skills_dir {
            None => Some(get_bundled_skills_dir()),
            Some(inner) => inner,
        };
        let package_manager = DefaultPackageManager::new(PackageManagerOptions {
            cwd: options.cwd.clone(),
            agent_dir: options.agent_dir.clone(),
            settings_manager: settings_manager.clone(),
            bundled_skills_dir: Some(bundled_skills_dir.clone()),
            extra_builtin_skill_overrides: options.extra_builtin_skill_overrides.clone(),
        });

        Self {
            cwd: options.cwd,
            agent_dir: options.agent_dir,
            settings_manager,
            event_bus: options.event_bus.unwrap_or_else(|| Arc::new(create_event_bus())),
            package_manager,
            bundled_skills_dir,
            additional_extension_paths: options.additional_extension_paths,
            additional_skill_paths: options.additional_skill_paths,
            additional_prompt_template_paths: options.additional_prompt_template_paths,
            additional_theme_paths: options.additional_theme_paths,
            extension_factories: options.extension_factories,
            no_extensions: options.no_extensions,
            no_skills: options.no_skills,
            no_prompt_templates: options.no_prompt_templates,
            no_themes: options.no_themes,
            no_context_files: options.no_context_files,
            system_prompt_source: options.system_prompt,
            append_system_prompt_source: options.append_system_prompt,
            extensions_override: options.extensions_override,
            skills_override: options.skills_override,
            prompts_override: options.prompts_override,
            themes_override: options.themes_override,
            agents_files_override: options.agents_files_override,
            system_prompt_override: options.system_prompt_override,
            append_system_prompt_override: options.append_system_prompt_override,
            extension_skill_source_infos: Mutex::new(HashMap::new()),
            extension_prompt_source_infos: Mutex::new(HashMap::new()),
            extension_theme_source_infos: Mutex::new(HashMap::new()),
            state: Mutex::new(LoaderState {
                runtime: Some(create_extension_runtime()),
                ..Default::default()
            }),
        }
    }

    /// The settings manager this loader (and its package manager) reads.
    pub fn settings_manager(&self) -> Arc<Mutex<SettingsManager>> {
        self.settings_manager.clone()
    }

    pub fn bundled_skills_dir(&self) -> Option<String> {
        self.bundled_skills_dir.clone()
    }

    /// `getLoadedExtensionPaths()`.
    pub fn get_loaded_extension_paths(&self) -> Vec<String> {
        self.state().loaded_extension_paths.clone()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, LoaderState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// `createSourceInfo(path, metadata)`.
///
/// `package-manager.ts` and `source-info.ts` declare `PathMetadata` separately in
/// Rust, so the loader converts between the two identical shapes here.
fn create_source_info_from_metadata(path: &str, metadata: &PathMetadata) -> SourceInfo {
    SourceInfo {
        path: path.to_string(),
        source: metadata.source.clone(),
        scope: metadata.scope.clone(),
        origin: metadata.origin.clone(),
        base_dir: metadata.base_dir.clone(),
    }
}

/// `join(a, b)` with `MAIN_SEPARATOR`, matching `utils/paths` join semantics.
fn join_path(base: &str, part: &str) -> String {
    let base = base.trim_end_matches(['/', '\\']);
    format!("{base}{}{part}", std::path::MAIN_SEPARATOR)
}

/// `resolve(p)` - Node's lexical resolution. `utils::paths` keeps its resolver
/// private, so the module carries the same lexical normalization locally.
fn resolve_path(value: &str) -> String {
    let path = Path::new(value);
    let absolute: PathBuf = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        if value.is_empty() {
            cwd
        } else {
            cwd.join(path)
        }
    };
    normalize_lexically(&absolute)
}

/// `resolve(a, b)`.
fn resolve_absolute(value: &str) -> String {
    let joined = if value.starts_with('/') || Path::new(value).is_absolute() {
        value.to_string()
    } else {
        join_path(&resolve_path("."), value)
    };
    normalize_lexically(Path::new(&joined))
}

fn normalize_lexically(path: &Path) -> String {
    use std::path::Component;
    let mut parts: Vec<String> = Vec::new();
    let mut prefix = String::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix_component) => {
                prefix.push_str(&prefix_component.as_os_str().to_string_lossy());
            }
            Component::RootDir => prefix.push(std::path::MAIN_SEPARATOR),
            Component::CurDir => {}
            Component::ParentDir => {
                if !parts.is_empty() {
                    parts.pop();
                }
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
        }
    }
    let joined = parts.join(&std::path::MAIN_SEPARATOR.to_string());
    if prefix.is_empty() {
        joined
    } else {
        format!("{prefix}{joined}")
    }
}

impl ResourceLoader for LoaderInner {
    fn get_extensions(&self) -> LoadExtensionsResult {
        let state = self.state();
        LoadExtensionsResult {
            extensions: state.extensions.clone(),
            errors: state.extension_errors.clone(),
            runtime: state.runtime.clone().unwrap_or_else(create_extension_runtime),
        }
    }

    fn get_skills(&self) -> SkillsResult {
        let state = self.state();
        SkillsResult {
            skills: state.skills.clone(),
            diagnostics: state.skill_diagnostics.clone(),
        }
    }

    fn get_prompts(&self) -> PromptsResult {
        let state = self.state();
        PromptsResult {
            prompts: state.prompts.clone(),
            diagnostics: state.prompt_diagnostics.clone(),
        }
    }

    fn get_themes(&self) -> ThemesResult {
        let state = self.state();
        ThemesResult {
            themes: state.themes.clone(),
            diagnostics: state.theme_diagnostics.clone(),
        }
    }

    fn get_agents_files(&self) -> AgentsFilesResult {
        AgentsFilesResult {
            agents_files: self.state().agents_files.clone(),
        }
    }

    fn get_system_prompt(&self) -> Option<String> {
        self.state().system_prompt.clone()
    }

    fn get_append_system_prompt(&self) -> Vec<String> {
        self.state().append_system_prompt.clone()
    }

    fn extend_resources(&self, paths: ResourceExtensionPaths) {
        let skill_paths = self.normalize_extension_paths(paths.skill_paths.unwrap_or_default());
        let prompt_paths = self.normalize_extension_paths(paths.prompt_paths.unwrap_or_default());
        let theme_paths = self.normalize_extension_paths(paths.theme_paths.unwrap_or_default());

        {
            let mut infos = self
                .extension_skill_source_infos
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for entry in &skill_paths {
                infos.insert(entry.path.clone(), create_source_info_from_metadata(&entry.path, &entry.metadata));
            }
        }
        {
            let mut infos = self
                .extension_prompt_source_infos
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for entry in &prompt_paths {
                infos.insert(entry.path.clone(), create_source_info_from_metadata(&entry.path, &entry.metadata));
            }
        }
        {
            let mut infos = self
                .extension_theme_source_infos
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for entry in &theme_paths {
                infos.insert(entry.path.clone(), create_source_info_from_metadata(&entry.path, &entry.metadata));
            }
        }

        if !skill_paths.is_empty() {
            let merged = self.merge_paths(
                &self.state().last_skill_paths,
                &skill_paths.iter().map(|entry| entry.path.clone()).collect::<Vec<_>>(),
            );
            self.state().last_skill_paths = merged.clone();
            self.update_skills_from_paths(&merged, None);
        }

        if !prompt_paths.is_empty() {
            let merged = self.merge_paths(
                &self.state().last_prompt_paths,
                &prompt_paths.iter().map(|entry| entry.path.clone()).collect::<Vec<_>>(),
            );
            self.state().last_prompt_paths = merged.clone();
            self.update_prompts_from_paths(&merged, None);
        }

        if !theme_paths.is_empty() {
            let merged = self.merge_paths(
                &self.state().last_theme_paths,
                &theme_paths.iter().map(|entry| entry.path.clone()).collect::<Vec<_>>(),
            );
            self.state().last_theme_paths = merged.clone();
            self.update_themes_from_paths(&merged, None);
        }
    }

    fn reload(&self) -> BoxFuture<'_, ()> {
        // The TypeScript mutates `this`; the Rust object owns its state behind a
        // mutex, so the reload future borrows the loader for its whole duration.
        async move {
            self.settings_manager.lock().expect("settings manager poisoned").reload_sync();
            self.reload_inner().await;
        }
        .boxed()
    }
}

/// `normalizeExtensionPaths(entries)`.
impl LoaderInner {
    fn normalize_extension_paths(&self, entries: Vec<ResourcePathEntry>) -> Vec<ResourcePathEntry> {
        entries
            .into_iter()
            .map(|entry| ResourcePathEntry {
                path: self.resolve_resource_path(&entry.path),
                metadata: entry.metadata,
            })
            .collect()
    }

    /// `mergePaths(primary, additional)`.
    fn merge_paths(&self, primary: &[String], additional: &[String]) -> Vec<String> {
        let mut merged: Vec<String> = Vec::new();
        let mut seen: Vec<String> = Vec::new();

        for p in primary.iter().chain(additional.iter()) {
            let resolved = self.resolve_resource_path(p);
            let canonical_path = canonicalize_path(&resolved);
            if seen.contains(&canonical_path) {
                continue;
            }
            seen.push(canonical_path);
            merged.push(resolved);
        }

        merged
    }

    /// `resolveResourcePath(p)`.
    fn resolve_resource_path(&self, p: &str) -> String {
        let trimmed = p.trim();
        let expanded = if trimmed == "~" {
            home_dir()
        } else if let Some(rest) = trimmed.strip_prefix("~/") {
            join_path(&home_dir(), rest)
        } else if let Some(rest) = trimmed.strip_prefix('~') {
            join_path(&home_dir(), rest)
        } else {
            trimmed.to_string()
        };
        resolve_absolute(&Path::new(&self.cwd).join(&expanded).to_string_lossy())
    }
}

fn home_dir() -> String {
    dirs::home_dir()
        .map(|home| home.to_string_lossy().to_string())
        .unwrap_or_default()
}

impl LoaderInner {
    /// The body of `reload()`.
    async fn reload_inner(&self) {
        let resolved_paths = match self.package_manager.resolve(None).await {
            Ok(paths) => paths,
            Err(error) => {
                self.state()
                    .skill_diagnostics
                    .push(ResourceDiagnostic {
                        diagnostic_type: RESOURCE_DIAGNOSTIC_ERROR.to_string(),
                        message: error,
                        path: None,
                        collision: None,
                    });
                ResolvedPaths::default()
            }
        };
        let cli_extension_paths = match self
            .package_manager
            .resolve_extension_sources(&self.additional_extension_paths, false, true)
            .await
        {
            Ok(paths) => paths,
            Err(error) => {
                self.state()
                    .skill_diagnostics
                    .push(ResourceDiagnostic {
                        diagnostic_type: RESOURCE_DIAGNOSTIC_ERROR.to_string(),
                        message: error,
                        path: None,
                        collision: None,
                    });
                ResolvedPaths::default()
            }
        };
        let mut metadata_by_path: Vec<(String, PathMetadata)> = Vec::new();

        self.extension_skill_source_infos
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.extension_prompt_source_infos
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.extension_theme_source_infos
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();

        // `getEnabledResources` - records metadata, returns the enabled entries.
        fn get_enabled_resources(
            resources: &[crate::core::package_manager::ResolvedResource],
            metadata_by_path: &mut Vec<(String, PathMetadata)>,
        ) -> Vec<crate::core::package_manager::ResolvedResource> {
            for resource in resources {
                if !metadata_by_path.iter().any(|(path, _)| *path == resource.path) {
                    metadata_by_path.push((resource.path.clone(), resource.metadata.clone()));
                }
            }
            resources.iter().filter(|resource| resource.enabled).cloned().collect()
        }

        fn get_enabled_paths(
            resources: &[crate::core::package_manager::ResolvedResource],
            metadata_by_path: &mut Vec<(String, PathMetadata)>,
        ) -> Vec<String> {
            get_enabled_resources(resources, metadata_by_path)
                .into_iter()
                .map(|resource| resource.path)
                .collect()
        }

        let enabled_extensions = get_enabled_paths(&resolved_paths.extensions, &mut metadata_by_path);
        let enabled_skill_resources = get_enabled_resources(&resolved_paths.skills, &mut metadata_by_path);
        let enabled_prompts = get_enabled_paths(&resolved_paths.prompts, &mut metadata_by_path);
        let enabled_themes = get_enabled_paths(&resolved_paths.themes, &mut metadata_by_path);

        // `mapSkillPath(resource)`: auto-discovered package directories map to SKILL.md.
        let mut enabled_skills: Vec<String> = Vec::new();
        for resource in enabled_skill_resources {
            let mut path = resource.path.clone();
            // `if (source !== "auto" && origin !== "package") return resource.path;`
            let auto_package =
                resource.metadata.source == "auto" && resource.metadata.origin == SOURCE_ORIGIN_PACKAGE;
            if auto_package {
                let is_directory = std::fs::metadata(&path)
                    .map(|metadata| metadata.is_dir())
                    .unwrap_or(false);
                if is_directory {
                    let skill_file = join_path(&path, "SKILL.md");
                    if Path::new(&skill_file).exists() {
                        if !metadata_by_path.iter().any(|(p, _)| *p == skill_file) {
                            metadata_by_path.push((skill_file.clone(), resource.metadata.clone()));
                        }
                        path = skill_file;
                    }
                }
            }
            enabled_skills.push(path);
        }

        // Add CLI paths metadata.
        for resource in &cli_extension_paths.extensions {
            if !metadata_by_path.iter().any(|(path, _)| *path == resource.path) {
                metadata_by_path.push((
                    resource.path.clone(),
                    PathMetadata {
                        source: "cli".to_string(),
                        scope: SOURCE_SCOPE_TEMPORARY.to_string(),
                        origin: SOURCE_ORIGIN_TOP_LEVEL.to_string(),
                        base_dir: None,
                    },
                ));
            }
        }
        for resource in &cli_extension_paths.skills {
            if !metadata_by_path.iter().any(|(path, _)| *path == resource.path) {
                metadata_by_path.push((
                    resource.path.clone(),
                    PathMetadata {
                        source: "cli".to_string(),
                        scope: SOURCE_SCOPE_TEMPORARY.to_string(),
                        origin: SOURCE_ORIGIN_TOP_LEVEL.to_string(),
                        base_dir: None,
                    },
                ));
            }
        }

        let cli_enabled_extensions = get_enabled_paths(&cli_extension_paths.extensions, &mut metadata_by_path);
        let cli_enabled_skills = get_enabled_paths(&cli_extension_paths.skills, &mut metadata_by_path);
        let cli_enabled_prompts = get_enabled_paths(&cli_extension_paths.prompts, &mut metadata_by_path);
        let cli_enabled_themes = get_enabled_paths(&cli_extension_paths.themes, &mut metadata_by_path);

        let extension_paths = if self.no_extensions {
            cli_enabled_extensions
        } else {
            self.merge_paths(&cli_enabled_extensions, &enabled_extensions)
        };

        let mut extensions_result =
            load_extensions(&extension_paths, &self.cwd, Some(self.event_bus.clone())).await;
        // Set before inline factories run so a factory can see which file-based
        // extensions actually loaded this cycle.
        self.state().loaded_extension_paths = extension_paths;

        let runtime = extensions_result.runtime.clone();
        let (inline_extensions, inline_errors) = self.load_extension_factories(runtime).await;
        extensions_result.extensions.extend(inline_extensions);
        extensions_result.errors.extend(inline_errors);

        // Detect extension conflicts; keep all extensions loaded.
        for conflict in self.detect_extension_conflicts(&extensions_result.extensions) {
            extensions_result.errors.push(LoadExtensionError {
                path: conflict.path,
                error: conflict.message,
            });
        }

        for p in &self.additional_extension_paths {
            if is_local_path(p) && !Path::new(p).exists() {
                extensions_result.errors.push(LoadExtensionError {
                    path: p.clone(),
                    error: format!("Extension path does not exist: {p}"),
                });
            }
        }

        if let Some(override_fn) = &self.extensions_override {
            extensions_result = override_fn(extensions_result);
        }
        self.apply_extension_source_info(&extensions_result.extensions, &metadata_by_path);

        let skill_paths = if self.no_skills {
            self.merge_paths(&cli_enabled_skills, &self.additional_skill_paths)
        } else {
            let primary: Vec<String> = cli_enabled_skills
                .iter()
                .chain(self.additional_skill_paths.iter())
                .cloned()
                .collect();
            self.merge_paths(&primary, &enabled_skills)
        };

        self.update_skills_from_paths(&skill_paths, Some(&metadata_by_path));
        {
            let mut state = self.state();
            state.last_skill_paths = skill_paths;
            // Surface resolution-time skill warnings (e.g. missing bundled skills dir).
            state.skill_diagnostics.extend(resolved_paths.diagnostics.clone());
            let mut extra_diagnostics: Vec<ResourceDiagnostic> = Vec::new();
            for p in &self.additional_skill_paths {
                if is_local_path(p)
                    && !Path::new(p).exists()
                    && !state.skill_diagnostics.iter().any(|d| d.path.as_deref() == Some(p.as_str()))
                {
                    extra_diagnostics.push(ResourceDiagnostic {
                        diagnostic_type: RESOURCE_DIAGNOSTIC_ERROR.to_string(),
                        message: "Skill path does not exist".to_string(),
                        path: Some(p.clone()),
                        collision: None,
                    });
                }
            }
            state.skill_diagnostics.extend(extra_diagnostics);
        }

        let prompt_paths = if self.no_prompt_templates {
            self.merge_paths(&cli_enabled_prompts, &self.additional_prompt_template_paths)
        } else {
            let primary: Vec<String> = cli_enabled_prompts
                .iter()
                .chain(enabled_prompts.iter())
                .cloned()
                .collect();
            self.merge_paths(&primary, &self.additional_prompt_template_paths)
        };

        self.update_prompts_from_paths(&prompt_paths, Some(&metadata_by_path));
        {
            let mut state = self.state();
            state.last_prompt_paths = prompt_paths;
            let mut extra_diagnostics: Vec<ResourceDiagnostic> = Vec::new();
            for p in &self.additional_prompt_template_paths {
                if is_local_path(p)
                    && !Path::new(p).exists()
                    && !state.prompt_diagnostics.iter().any(|d| d.path.as_deref() == Some(p.as_str()))
                {
                    extra_diagnostics.push(ResourceDiagnostic {
                        diagnostic_type: RESOURCE_DIAGNOSTIC_ERROR.to_string(),
                        message: "Prompt template path does not exist".to_string(),
                        path: Some(p.clone()),
                        collision: None,
                    });
                }
            }
            state.prompt_diagnostics.extend(extra_diagnostics);
        }

        let theme_paths = if self.no_themes {
            self.merge_paths(&cli_enabled_themes, &self.additional_theme_paths)
        } else {
            let primary: Vec<String> = cli_enabled_themes
                .iter()
                .chain(enabled_themes.iter())
                .cloned()
                .collect();
            self.merge_paths(&primary, &self.additional_theme_paths)
        };

        self.update_themes_from_paths(&theme_paths, Some(&metadata_by_path));
        {
            let mut state = self.state();
            state.last_theme_paths = theme_paths;
            let mut extra_diagnostics: Vec<ResourceDiagnostic> = Vec::new();
            for p in &self.additional_theme_paths {
                if !Path::new(p).exists()
                    && !state.theme_diagnostics.iter().any(|d| d.path.as_deref() == Some(p.as_str()))
                {
                    extra_diagnostics.push(ResourceDiagnostic {
                        diagnostic_type: RESOURCE_DIAGNOSTIC_ERROR.to_string(),
                        message: "Theme path does not exist".to_string(),
                        path: Some(p.clone()),
                        collision: None,
                    });
                }
            }
            state.theme_diagnostics.extend(extra_diagnostics);
        }

        let agents_files = AgentsFilesResult {
            agents_files: if self.no_context_files {
                Vec::new()
            } else {
                load_project_context_files(&self.cwd, &self.agent_dir)
            },
        };
        let resolved_agents_files = match &self.agents_files_override {
            Some(override_fn) => override_fn(agents_files),
            None => agents_files,
        };

        let base_system_prompt = resolve_prompt_input(
            self.system_prompt_source
                .clone()
                .or_else(|| self.discover_system_prompt_file()),
            "system prompt",
        );
        let system_prompt = match &self.system_prompt_override {
            Some(override_fn) => override_fn(base_system_prompt),
            None => base_system_prompt,
        };

        let append_sources = self.append_system_prompt_source.clone().unwrap_or_else(|| {
            self.discover_append_system_prompt_file()
                .map(|path| vec![path])
                .unwrap_or_default()
        });
        let base_append: Vec<String> = append_sources
            .iter()
            .filter_map(|source| resolve_prompt_input(Some(source.clone()), "append system prompt"))
            .collect();
        let append_system_prompt = match &self.append_system_prompt_override {
            Some(override_fn) => override_fn(base_append),
            None => base_append,
        };

        let mut state = self.state();
        state.extensions = extensions_result.extensions;
        state.extension_errors = extensions_result.errors;
        state.runtime = Some(extensions_result.runtime);
        state.agents_files = resolved_agents_files.agents_files;
        state.system_prompt = system_prompt;
        state.append_system_prompt = append_system_prompt;
    }
}

impl LoaderInner {
    /// `updateSkillsFromPaths(skillPaths, metadataByPath?)`.
    fn update_skills_from_paths(&self, skill_paths: &[String], metadata_by_path: Option<&Vec<(String, PathMetadata)>>) {
        let skills_result = if self.no_skills && skill_paths.is_empty() {
            SkillsResult::default()
        } else {
            let loaded = load_skills(&LoadSkillsOptions {
                cwd: self.cwd.clone(),
                agent_dir: self.agent_dir.clone(),
                skill_paths: skill_paths.to_vec(),
                include_defaults: false,
            });
            SkillsResult {
                skills: loaded.skills,
                diagnostics: loaded.diagnostics,
            }
        };
        let resolved_skills = match &self.skills_override {
            Some(override_fn) => override_fn(skills_result),
            None => skills_result,
        };
        let extension_infos = self
            .extension_skill_source_infos
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let skills: Vec<Skill> = resolved_skills
            .skills
            .into_iter()
            .map(|skill| {
                let file_path = skill.file_path().to_string();
                let source_info = self
                    .find_source_info_for_path(&file_path, Some(&extension_infos), metadata_by_path)
                    .unwrap_or_else(|| {
                        skill_source_info(&skill)
                            .unwrap_or_else(|| self.get_default_source_info_for_path(&file_path))
                    });
                with_skill_source_info(skill, source_info)
            })
            .collect();
        let mut state = self.state();
        state.skills = skills;
        state.skill_diagnostics = resolved_skills.diagnostics;
    }

    /// `updatePromptsFromPaths(promptPaths, metadataByPath?)`.
    fn update_prompts_from_paths(&self, prompt_paths: &[String], metadata_by_path: Option<&Vec<(String, PathMetadata)>>) {
        let prompts_result = if self.no_prompt_templates && prompt_paths.is_empty() {
            PromptsResult::default()
        } else {
            let all_prompts = load_prompt_templates(&LoadPromptTemplatesOptions {
                cwd: self.cwd.clone(),
                agent_dir: self.agent_dir.clone(),
                prompt_paths: prompt_paths.to_vec(),
                include_defaults: false,
            });
            dedupe_prompts(all_prompts)
        };
        let resolved_prompts = match &self.prompts_override {
            Some(override_fn) => override_fn(prompts_result),
            None => prompts_result,
        };
        let extension_infos = self
            .extension_prompt_source_infos
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let prompts: Vec<PromptTemplate> = resolved_prompts
            .prompts
            .into_iter()
            .map(|mut prompt| {
                prompt.source_info = self
                    .find_source_info_for_path(&prompt.file_path, Some(&extension_infos), metadata_by_path)
                    .unwrap_or_else(|| self.get_default_source_info_for_path(&prompt.file_path));
                prompt
            })
            .collect();
        let mut state = self.state();
        state.prompts = prompts;
        state.prompt_diagnostics = resolved_prompts.diagnostics;
    }

    /// `updateThemesFromPaths(themePaths, metadataByPath?)`.
    fn update_themes_from_paths(&self, theme_paths: &[String], metadata_by_path: Option<&Vec<(String, PathMetadata)>>) {
        let mut themes_result = if self.no_themes && theme_paths.is_empty() {
            OwnedThemesResult::default()
        } else {
            let loaded = self.load_themes(theme_paths, false);
            let deduped = dedupe_themes(loaded.themes);
            OwnedThemesResult {
                themes: deduped.themes,
                diagnostics: loaded
                    .diagnostics
                    .into_iter()
                    .chain(deduped.diagnostics)
                    .collect(),
            }
        };
        if let Some(override_fn) = &self.themes_override {
            themes_result = override_fn(themes_result);
        }
        let extension_infos = self
            .extension_theme_source_infos
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let themes: Vec<Arc<Theme>> = themes_result
            .themes
            .into_iter()
            .map(|mut theme| {
                if let Some(source_path) = theme.source_path.clone() {
                    if let Some(info) = self.find_source_info_for_path(&source_path, Some(&extension_infos), metadata_by_path) {
                        // `theme.sourceInfo` is the theme module's own three-field shape.
                        theme.source_info = Some(crate::modes::interactive::theme::theme::SourceInfo {
                            path: info.path,
                            source: info.source,
                            scope: info.scope,
                        });
                    }
                }
                Arc::new(theme)
            })
            .collect();
        let mut state = self.state();
        state.themes = themes;
        state.theme_diagnostics = themes_result.diagnostics;
    }

    /// `applyExtensionSourceInfo(extensions, metadataByPath)`.
    fn apply_extension_source_info(
        &self,
        extensions: &[SharedExtension],
        metadata_by_path: &[(String, PathMetadata)],
    ) {
        for extension in extensions {
            let mut extension = extension.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let metadata_by_path = metadata_by_path.to_vec();
            let source_info = self
                .find_source_info_for_path(&extension.path, None, Some(&metadata_by_path))
                .unwrap_or_else(|| self.get_default_source_info_for_path(&extension.path));
            extension.source_info = source_info.clone();
            for command in extension.commands.values_mut() {
                command.source_info = source_info.clone();
            }
            for tool in extension.tools.values_mut() {
                tool.source_info = source_info.clone();
            }
        }
    }

    /// `findSourceInfoForPath(resourcePath, extraSourceInfos?, metadataByPath?)`.
    fn find_source_info_for_path(
        &self,
        resource_path: &str,
        extra_source_infos: Option<&HashMap<String, SourceInfo>>,
        metadata_by_path: Option<&Vec<(String, PathMetadata)>>,
    ) -> Option<SourceInfo> {
        if resource_path.is_empty() {
            return None;
        }

        if resource_path.starts_with('<') {
            return Some(self.get_default_source_info_for_path(resource_path));
        }

        let normalized_resource_path = resolve_absolute(resource_path);
        if let Some(extra_source_infos) = extra_source_infos {
            for (source_path, source_info) in extra_source_infos {
                let normalized_source_path = resolve_absolute(source_path);
                if normalized_resource_path == normalized_source_path
                    || normalized_resource_path.starts_with(&format!("{normalized_source_path}{}", std::path::MAIN_SEPARATOR))
                {
                    let mut info = source_info.clone();
                    info.path = resource_path.to_string();
                    return Some(info);
                }
            }
        }

        if let Some(metadata_by_path) = metadata_by_path {
            if let Some((_, metadata)) = metadata_by_path
                .iter()
                .find(|(path, _)| *path == normalized_resource_path || path == resource_path)
            {
                return Some(create_source_info_from_metadata(resource_path, metadata));
            }

            for (source_path, metadata) in metadata_by_path {
                let normalized_source_path = resolve_absolute(source_path);
                if normalized_resource_path == normalized_source_path
                    || normalized_resource_path.starts_with(&format!("{normalized_source_path}{}", std::path::MAIN_SEPARATOR))
                {
                    return Some(create_source_info_from_metadata(resource_path, metadata));
                }
            }
        }

        None
    }

    /// `getDefaultSourceInfoForPath(filePath)`.
    fn get_default_source_info_for_path(&self, file_path: &str) -> SourceInfo {
        if file_path.starts_with('<') && file_path.ends_with('>') {
            let inner = &file_path[1..file_path.len() - 1];
            let source = match inner.split(':').next() {
                Some(part) if !part.is_empty() => part.to_string(),
                _ => "temporary".to_string(),
            };
            return SourceInfo {
                path: file_path.to_string(),
                source,
                scope: SOURCE_SCOPE_TEMPORARY.to_string(),
                origin: SOURCE_ORIGIN_TOP_LEVEL.to_string(),
                base_dir: None,
            };
        }

        let normalized_path = resolve_absolute(file_path);
        let agent_roots = [
            join_path(&self.agent_dir, "skills"),
            join_path(&self.agent_dir, "prompts"),
            join_path(&self.agent_dir, "themes"),
            join_path(&self.agent_dir, "extensions"),
        ];
        let project_roots = [
            join_path(&join_path(&self.cwd, CONFIG_DIR_NAME), "skills"),
            join_path(&join_path(&self.cwd, CONFIG_DIR_NAME), "prompts"),
            join_path(&join_path(&self.cwd, CONFIG_DIR_NAME), "themes"),
            join_path(&join_path(&self.cwd, CONFIG_DIR_NAME), "extensions"),
        ];

        for root in agent_roots {
            if is_under_path(&normalized_path, &root) {
                return SourceInfo {
                    path: file_path.to_string(),
                    source: "local".to_string(),
                    scope: SOURCE_SCOPE_USER.to_string(),
                    origin: SOURCE_ORIGIN_TOP_LEVEL.to_string(),
                    base_dir: Some(resolve_absolute(&root)),
                };
            }
        }

        for root in project_roots {
            if is_under_path(&normalized_path, &root) {
                return SourceInfo {
                    path: file_path.to_string(),
                    source: "local".to_string(),
                    scope: SOURCE_SCOPE_PROJECT.to_string(),
                    origin: SOURCE_ORIGIN_TOP_LEVEL.to_string(),
                    base_dir: Some(resolve_absolute(&root)),
                };
            }
        }

        let base_dir = match std::fs::metadata(&normalized_path) {
            Ok(metadata) if metadata.is_dir() => normalized_path.clone(),
            _ => parent_of(&normalized_path),
        };
        SourceInfo {
            path: file_path.to_string(),
            source: "local".to_string(),
            scope: SOURCE_SCOPE_TEMPORARY.to_string(),
            origin: SOURCE_ORIGIN_TOP_LEVEL.to_string(),
            base_dir: Some(base_dir),
        }
    }
}

fn parent_of(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// `SourceInfo` of a loaded skill, if the loader produced one.
fn skill_source_info(skill: &Skill) -> Option<SourceInfo> {
    match skill {
        Skill::Markdown(skill) => Some(skill.base.source_info.clone()),
        Skill::Python(skill) => Some(skill.base.source_info.clone()),
    }
}

/// Replace a skill's `sourceInfo` (the `{...skill, sourceInfo}` spread).
fn with_skill_source_info(skill: Skill, source_info: SourceInfo) -> Skill {
    match skill {
        Skill::Markdown(mut skill) => {
            skill.base.source_info = source_info;
            Skill::Markdown(skill)
        }
        Skill::Python(mut skill) => {
            skill.base.source_info = source_info;
            Skill::Python(skill)
        }
    }
}

/// `isUnderPath(target, root)`.
fn is_under_path(target: &str, root: &str) -> bool {
    let normalized_root = resolve_absolute(root);
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

impl LoaderInner {
    /// `loadThemes(paths, includeDefaults = true)`.
    fn load_themes(&self, paths: &[String], include_defaults: bool) -> OwnedThemesResult {
        let mut themes: Vec<Theme> = Vec::new();
        let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
        if include_defaults {
            let default_dirs = [
                join_path(&self.agent_dir, "themes"),
                join_path(&join_path(&self.cwd, CONFIG_DIR_NAME), "themes"),
            ];
            for dir in default_dirs {
                load_themes_from_dir(&dir, &mut themes, &mut diagnostics);
            }
        }

        for p in paths {
            let resolved = resolve_absolute(&Path::new(&self.cwd).join(p).to_string_lossy());
            if !Path::new(&resolved).exists() {
                diagnostics.push(ResourceDiagnostic {
                    diagnostic_type: RESOURCE_DIAGNOSTIC_WARNING.to_string(),
                    message: "theme path does not exist".to_string(),
                    path: Some(resolved),
                    collision: None,
                });
                continue;
            }

            match std::fs::metadata(&resolved) {
                Ok(metadata) if metadata.is_dir() => {
                    load_themes_from_dir(&resolved, &mut themes, &mut diagnostics);
                }
                Ok(metadata) if metadata.is_file() && resolved.ends_with(".json") => {
                    load_theme_from_file(&resolved, &mut themes, &mut diagnostics);
                }
                Ok(_) => diagnostics.push(ResourceDiagnostic {
                    diagnostic_type: RESOURCE_DIAGNOSTIC_WARNING.to_string(),
                    message: "theme path is not a json file".to_string(),
                    path: Some(resolved),
                    collision: None,
                }),
                Err(error) => diagnostics.push(ResourceDiagnostic {
                    diagnostic_type: RESOURCE_DIAGNOSTIC_WARNING.to_string(),
                    message: error.to_string(),
                    path: Some(resolved),
                    collision: None,
                }),
            }
        }

        OwnedThemesResult { themes, diagnostics }
    }

    fn discover_system_prompt_file(&self) -> Option<String> {
        let project_path = join_path(&join_path(&self.cwd, CONFIG_DIR_NAME), "SYSTEM.md");
        if Path::new(&project_path).exists() {
            return Some(project_path);
        }

        let global_path = join_path(&self.agent_dir, "SYSTEM.md");
        if Path::new(&global_path).exists() {
            return Some(global_path);
        }

        None
    }

    fn discover_append_system_prompt_file(&self) -> Option<String> {
        let project_path = join_path(&join_path(&self.cwd, CONFIG_DIR_NAME), "APPEND_SYSTEM.md");
        if Path::new(&project_path).exists() {
            return Some(project_path);
        }

        let global_path = join_path(&self.agent_dir, "APPEND_SYSTEM.md");
        if Path::new(&global_path).exists() {
            return Some(global_path);
        }

        None
    }

    /// `loadExtensionFactories(runtime)`.
    async fn load_extension_factories(
        &self,
        runtime: ExtensionRuntime,
    ) -> (Vec<SharedExtension>, Vec<LoadExtensionError>) {
        let mut extensions: Vec<SharedExtension> = Vec::new();
        let mut errors: Vec<LoadExtensionError> = Vec::new();

        for (index, factory) in self.extension_factories.iter().enumerate() {
            let extension_path = format!("<inline:{}>", index + 1);
            match load_extension_from_factory(
                factory.clone(),
                &self.cwd,
                self.event_bus.clone(),
                runtime.clone(),
                Some(&extension_path),
            )
            .await
            {
                Ok(extension) => extensions.push(extension),
                Err(error) => errors.push(LoadExtensionError {
                    path: extension_path,
                    error,
                }),
            }
        }

        (extensions, errors)
    }

    /// `detectExtensionConflicts(extensions)`.
    fn detect_extension_conflicts(&self, extensions: &[SharedExtension]) -> Vec<ConflictEntry> {
        let mut conflicts: Vec<ConflictEntry> = Vec::new();
        let mut tool_owners: Vec<(String, String)> = Vec::new();
        let mut flag_owners: Vec<(String, String)> = Vec::new();

        for ext in extensions {
            let ext = ext.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for tool_name in ext.tools.keys() {
                match tool_owners.iter().find(|(name, _)| name == tool_name) {
                    Some((_, owner)) if *owner != ext.path => conflicts.push(ConflictEntry {
                        path: ext.path.clone(),
                        message: format!("Tool \"{tool_name}\" conflicts with {owner}"),
                    }),
                    Some(_) => {}
                    None => tool_owners.push((tool_name.clone(), ext.path.clone())),
                }
            }

            for flag_name in ext.flags.keys() {
                match flag_owners.iter().find(|(name, _)| name == flag_name) {
                    Some((_, owner)) if *owner != ext.path => conflicts.push(ConflictEntry {
                        path: ext.path.clone(),
                        message: format!("Flag \"--{flag_name}\" conflicts with {owner}"),
                    }),
                    Some(_) => {}
                    None => flag_owners.push((flag_name.clone(), ext.path.clone())),
                }
            }
        }

        conflicts
    }
}

/// `{ path; message }` conflict entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ConflictEntry {
    pub path: String,
    pub message: String,
}

/// `loadThemesFromDir(dir, themes, diagnostics)`.
fn load_themes_from_dir(dir: &str, themes: &mut Vec<Theme>, diagnostics: &mut Vec<ResourceDiagnostic>) {
    if !Path::new(dir).exists() {
        return;
    }

    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let entry_path = entry.path();
                let mut is_file = entry.file_type().map(|file_type| file_type.is_file()).unwrap_or(false);
                let is_symlink = std::fs::symlink_metadata(&entry_path)
                    .map(|metadata| metadata.file_type().is_symlink())
                    .unwrap_or(false);
                if is_symlink {
                    match std::fs::metadata(&entry_path) {
                        Ok(metadata) => is_file = metadata.is_file(),
                        Err(_) => continue,
                    }
                }
                if !is_file {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.ends_with(".json") {
                    continue;
                }
                load_theme_from_file(&join_path(dir, &name), themes, diagnostics);
            }
        }
        Err(error) => diagnostics.push(ResourceDiagnostic {
            diagnostic_type: RESOURCE_DIAGNOSTIC_WARNING.to_string(),
            message: error.to_string(),
            path: Some(dir.to_string()),
            collision: None,
        }),
    }
}

/// `loadThemeFromFile(filePath, themes, diagnostics)`.
fn load_theme_from_file(file_path: &str, themes: &mut Vec<Theme>, diagnostics: &mut Vec<ResourceDiagnostic>) {
    match load_theme_from_path(file_path, None) {
        Ok(theme) => themes.push(theme),
        Err(message) => diagnostics.push(ResourceDiagnostic {
            diagnostic_type: RESOURCE_DIAGNOSTIC_WARNING.to_string(),
            message,
            path: Some(file_path.to_string()),
            collision: None,
        }),
    }
}

/// `dedupePrompts(prompts)`.
fn dedupe_prompts(prompts: Vec<PromptTemplate>) -> PromptsResult {
    let mut seen: Vec<(String, PromptTemplate)> = Vec::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    for prompt in prompts {
        match seen.iter().find(|(name, _)| *name == prompt.name) {
            Some((_, existing)) => diagnostics.push(ResourceDiagnostic {
                diagnostic_type: RESOURCE_DIAGNOSTIC_COLLISION.to_string(),
                message: format!("name \"/{}\" collision", prompt.name),
                path: Some(prompt.file_path.clone()),
                collision: Some(crate::core::diagnostics::ResourceCollision {
                    resource_type: crate::core::diagnostics::RESOURCE_TYPE_PROMPT.to_string(),
                    name: prompt.name.clone(),
                    winner_path: existing.file_path.clone(),
                    loser_path: prompt.file_path.clone(),
                    winner_source: None,
                    loser_source: None,
                }),
            }),
            None => seen.push((prompt.name.clone(), prompt)),
        }
    }

    PromptsResult {
        prompts: seen.into_iter().map(|(_, prompt)| prompt).collect(),
        diagnostics,
    }
}

/// `dedupeThemes(themes)`.
fn dedupe_themes(themes: Vec<Theme>) -> OwnedThemesResult {
    let mut seen: Vec<(String, Theme)> = Vec::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    for theme in themes {
        let name = theme.name.clone().unwrap_or_else(|| "unnamed".to_string());
        match seen.iter().find(|(existing, _)| *existing == name) {
            Some((_, existing)) => diagnostics.push(ResourceDiagnostic {
                diagnostic_type: RESOURCE_DIAGNOSTIC_COLLISION.to_string(),
                message: format!("name \"{name}\" collision"),
                path: theme.source_path.clone(),
                collision: Some(crate::core::diagnostics::ResourceCollision {
                    resource_type: crate::core::diagnostics::RESOURCE_TYPE_THEME.to_string(),
                    name: name.clone(),
                    winner_path: existing
                        .source_path
                        .clone()
                        .unwrap_or_else(|| "<builtin>".to_string()),
                    loser_path: theme
                        .source_path
                        .clone()
                        .unwrap_or_else(|| "<builtin>".to_string()),
                    winner_source: None,
                    loser_source: None,
                }),
            }),
            None => seen.push((name, theme)),
        }
    }

    OwnedThemesResult {
        themes: seen.into_iter().map(|(_, theme)| theme).collect(),
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rl-rs-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }

    fn loader(cwd: &str, agent_dir: &str) -> LoaderInner {
        LoaderInner::new(DefaultResourceLoaderOptions {
            // No bundled skills: the tests assert the local source set only.
            bundled_skills_dir: Some(None),
            ..DefaultResourceLoaderOptions::new(cwd, agent_dir)
        })
    }

    #[test]
    fn starts_empty_before_reload() {
        let root = temp_dir("empty");
        let loader = loader(
            &root.join("project").to_string_lossy(),
            &root.join("agent").to_string_lossy(),
        );

        assert!(loader.get_extensions().extensions.is_empty());
        assert!(loader.get_skills().skills.is_empty());
        assert!(loader.get_prompts().prompts.is_empty());
        assert!(loader.get_themes().themes.is_empty());
        assert!(loader.get_agents_files().agents_files.is_empty());
        assert_eq!(loader.get_system_prompt(), None);
        assert!(loader.get_append_system_prompt().is_empty());
    }

    #[tokio::test]
    async fn discovers_skills_and_prompts_from_agent_dir() {
        let root = temp_dir("discover");
        let agent_dir = root.join("agent");
        let cwd = root.join("project");
        write_file(
            &agent_dir.join("skills").join("test-skill.md"),
            "---\nname: test-skill\ndescription: A test skill\n---\nSkill content here.",
        );
        write_file(
            &agent_dir.join("prompts").join("test-prompt.md"),
            "---\ndescription: A test prompt\n---\nPrompt content.",
        );

        let loader = loader(&cwd.to_string_lossy(), &agent_dir.to_string_lossy());
        loader.reload().await;

        let skills = loader.get_skills();
        assert!(skills.skills.iter().any(|skill| skill.name() == "test-skill"));
        let prompts = loader.get_prompts();
        assert!(prompts.prompts.iter().any(|prompt| prompt.name == "test-prompt"));
    }

    #[tokio::test]
    async fn prefers_project_resources_over_user_on_name_collisions() {
        let root = temp_dir("collision");
        let agent_dir = root.join("agent");
        let cwd = root.join("project");
        write_file(&agent_dir.join("prompts").join("commit.md"), "User prompt");
        write_file(&cwd.join(".prime").join("agent").join("prompts").join("commit.md"), "Project prompt");

        let loader = loader(&cwd.to_string_lossy(), &agent_dir.to_string_lossy());
        loader.reload().await;

        let prompts = loader.get_prompts();
        let commit = prompts
            .prompts
            .iter()
            .find(|prompt| prompt.name == "commit")
            .expect("commit prompt");
        assert_eq!(commit.content, "Project prompt");
    }

    #[tokio::test]
    async fn discovers_agents_md_context_files() {
        let root = temp_dir("agents");
        let agent_dir = root.join("agent");
        let cwd = root.join("project");
        write_file(&agent_dir.join("AGENTS.md"), "Global instructions");
        write_file(&cwd.join("AGENTS.md"), "Project instructions");

        let loader = loader(&cwd.to_string_lossy(), &agent_dir.to_string_lossy());
        loader.reload().await;

        let files = loader.get_agents_files().agents_files;
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].content, "Global instructions");
        assert_eq!(files[1].content, "Project instructions");
    }

    #[tokio::test]
    async fn skips_context_files_when_no_context_files_is_true() {
        let root = temp_dir("nocontext");
        let agent_dir = root.join("agent");
        let cwd = root.join("project");
        write_file(&cwd.join("AGENTS.md"), "Project instructions");

        let loader = LoaderInner::new(DefaultResourceLoaderOptions {
            no_context_files: true,
            bundled_skills_dir: Some(None),
            ..DefaultResourceLoaderOptions::new(&cwd.to_string_lossy(), &agent_dir.to_string_lossy())
        });
        loader.reload().await;

        assert!(loader.get_agents_files().agents_files.is_empty());
    }

    #[tokio::test]
    async fn discovers_system_and_append_system_prompts() {
        let root = temp_dir("system");
        let agent_dir = root.join("agent");
        let cwd = root.join("project");
        write_file(
            &cwd.join(".prime").join("agent").join("SYSTEM.md"),
            "Custom system prompt",
        );
        write_file(
            &agent_dir.join("APPEND_SYSTEM.md"),
            "Extra instructions",
        );

        let loader = loader(&cwd.to_string_lossy(), &agent_dir.to_string_lossy());
        loader.reload().await;

        assert_eq!(loader.get_system_prompt(), Some("Custom system prompt".to_string()));
        assert_eq!(
            loader.get_append_system_prompt(),
            vec!["Extra instructions".to_string()]
        );
    }

    #[tokio::test]
    async fn applies_system_prompt_and_append_overrides() {
        let root = temp_dir("overrides");
        let agent_dir = root.join("agent");
        let cwd = root.join("project");

        let loader = LoaderInner::new(DefaultResourceLoaderOptions {
            bundled_skills_dir: Some(None),
            system_prompt: Some("Base".to_string()),
            system_prompt_override: Some(Arc::new(|base: Option<String>| {
                base.map(|base| format!("{base} + override"))
            })),
            append_system_prompt: Some(vec!["Base append".to_string()]),
            append_system_prompt_override: Some(Arc::new(|base: Vec<String>| {
                base.into_iter().map(|entry| format!("{entry} + override")).collect()
            })),
            ..DefaultResourceLoaderOptions::new(&cwd.to_string_lossy(), &agent_dir.to_string_lossy())
        });
        loader.reload().await;

        assert_eq!(loader.get_system_prompt(), Some("Base + override".to_string()));
        assert_eq!(
            loader.get_append_system_prompt(),
            vec!["Base append + override".to_string()]
        );
    }

    #[tokio::test]
    async fn skills_override_replaces_the_loaded_set() {
        let root = temp_dir("skillsoverride");
        let agent_dir = root.join("agent");
        let cwd = root.join("project");
        write_file(
            &agent_dir.join("skills").join("kept.md"),
            "---\nname: kept\ndescription: kept\n---\nbody",
        );

        let loader = LoaderInner::new(DefaultResourceLoaderOptions {
            bundled_skills_dir: Some(None),
            skills_override: Some(Arc::new(|base: SkillsResult| SkillsResult {
                skills: base.skills.into_iter().filter(|skill| skill.name() != "kept").collect(),
                diagnostics: base.diagnostics,
            })),
            ..DefaultResourceLoaderOptions::new(&cwd.to_string_lossy(), &agent_dir.to_string_lossy())
        });
        loader.reload().await;

        assert!(!loader.get_skills().skills.iter().any(|skill| skill.name() == "kept"));
    }

    #[tokio::test]
    async fn no_skills_still_loads_additional_skill_paths() {
        let root = temp_dir("noskills");
        let agent_dir = root.join("agent");
        let cwd = root.join("project");
        write_file(
            &agent_dir.join("skills").join("auto.md"),
            "---\nname: auto\ndescription: auto\n---\nbody",
        );
        let extra = root.join("extra.md");
        write_file(&extra, "---\nname: extra\ndescription: extra\n---\nbody");

        let loader = LoaderInner::new(DefaultResourceLoaderOptions {
            no_skills: true,
            bundled_skills_dir: Some(None),
            additional_skill_paths: vec![extra.to_string_lossy().to_string()],
            ..DefaultResourceLoaderOptions::new(&cwd.to_string_lossy(), &agent_dir.to_string_lossy())
        });
        loader.reload().await;

        let skills = loader.get_skills().skills;
        assert!(skills.iter().any(|skill| skill.name() == "extra"));
        assert!(!skills.iter().any(|skill| skill.name() == "auto"));
    }

    #[tokio::test]
    async fn extend_resources_loads_skills_with_extension_metadata() {
        let root = temp_dir("extend");
        let agent_dir = root.join("agent");
        let cwd = root.join("project");
        let skill_dir = root.join("extension-skills");
        write_file(
            &skill_dir.join("SKILL.md"),
            "---\nname: extension-skill\ndescription: from an extension\n---\nbody",
        );

        let loader = loader(&cwd.to_string_lossy(), &agent_dir.to_string_lossy());
        loader.reload().await;
        loader.extend_resources(ResourceExtensionPaths {
            skill_paths: Some(vec![ResourcePathEntry {
                path: skill_dir.to_string_lossy().to_string(),
                metadata: PathMetadata {
                    source: "npm:@acme/ext".to_string(),
                    scope: SOURCE_SCOPE_USER.to_string(),
                    origin: SOURCE_ORIGIN_PACKAGE.to_string(),
                    base_dir: None,
                },
            }]),
            ..Default::default()
        });

        let skills = loader.get_skills().skills;
        let skill = skills
            .iter()
            .find(|skill| skill.name() == "extension-skill")
            .expect("extension skill");
        assert_eq!(skill_source_info(skill).unwrap().source, "npm:@acme/ext");
    }

    #[test]
    fn detects_flag_conflicts_between_extensions() {
        let root = temp_dir("conflicts");
        let loader = loader(
            &root.join("project").to_string_lossy(),
            &root.join("agent").to_string_lossy(),
        );
        fn extension(path: &str) -> crate::core::extensions::types::Extension {
            crate::core::extensions::types::Extension {
                path: path.to_string(),
                resolved_path: path.to_string(),
                source_info: SourceInfo {
                    path: path.to_string(),
                    source: "local".to_string(),
                    scope: SOURCE_SCOPE_TEMPORARY.to_string(),
                    origin: SOURCE_ORIGIN_TOP_LEVEL.to_string(),
                    base_dir: None,
                },
                handlers: HashMap::new(),
                tools: HashMap::new(),
                message_renderers: HashMap::new(),
                commands: Default::default(),
                flags: HashMap::new(),
                shortcuts: HashMap::new(),
            }
        }
        fn flag(name: &str, owner: &str) -> crate::core::extensions::types::ExtensionFlag {
            crate::core::extensions::types::ExtensionFlag {
                name: name.to_string(),
                description: None,
                flag_type: "string".to_string(),
                default: None,
                extension_path: owner.to_string(),
            }
        }

        let mut first = extension("a");
        let mut second = extension("b");
        first.flags.insert("shared".to_string(), flag("shared", "a"));
        second.flags.insert("shared".to_string(), flag("shared", "b"));
        let extensions = vec![Arc::new(Mutex::new(first)), Arc::new(Mutex::new(second))];

        let conflicts = loader.detect_extension_conflicts(&extensions);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].path, "b");
        assert_eq!(conflicts[0].message, "Flag \"--shared\" conflicts with a");
    }
}

/// `class DefaultResourceLoader` - a shared handle to one loader instance.
///
/// The TypeScript class is the loader itself; Rust splits the handle (cheap to
/// clone, `Arc`-shared) from [`LoaderInner`], the state `reload()` mutates.
#[derive(Clone)]
pub struct DefaultResourceLoader {
    inner: Arc<LoaderInner>,
}

impl std::fmt::Debug for DefaultResourceLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(f)
    }
}

impl DefaultResourceLoader {
    pub fn new(options: DefaultResourceLoaderOptions) -> Self {
        Self {
            inner: Arc::new(LoaderInner::new(options)),
        }
    }

    /// The shared state this handle points at.
    pub fn inner(&self) -> &Arc<LoaderInner> {
        &self.inner
    }

    /// `getExtensions()`.
    pub fn get_extensions(&self) -> LoadExtensionsResult {
        self.inner.get_extensions()
    }

    /// `reload()` as an inherent method, so call sites that hold the concrete
    /// loader (not the shared trait object) resolve without importing a trait.
    pub async fn reload(&self) {
        self.inner.reload().await;
    }

    /// `getLoadedExtensionPaths()`.
    pub fn get_loaded_extension_paths(&self) -> Vec<String> {
        self.inner.get_loaded_extension_paths()
    }

    pub fn settings_manager(&self) -> Arc<Mutex<SettingsManager>> {
        self.inner.settings_manager()
    }

    pub fn bundled_skills_dir(&self) -> Option<String> {
        self.inner.bundled_skills_dir()
    }
}

impl ResourceLoader for DefaultResourceLoader {
    fn get_extensions(&self) -> LoadExtensionsResult {
        self.inner.get_extensions()
    }

    fn get_skills(&self) -> SkillsResult {
        self.inner.get_skills()
    }

    fn get_prompts(&self) -> PromptsResult {
        self.inner.get_prompts()
    }

    fn get_themes(&self) -> ThemesResult {
        self.inner.get_themes()
    }

    fn get_agents_files(&self) -> AgentsFilesResult {
        self.inner.get_agents_files()
    }

    fn get_system_prompt(&self) -> Option<String> {
        self.inner.get_system_prompt()
    }

    fn get_append_system_prompt(&self) -> Vec<String> {
        self.inner.get_append_system_prompt()
    }

    fn extend_resources(&self, paths: ResourceExtensionPaths) {
        self.inner.extend_resources(paths)
    }

    fn reload(&self) -> BoxFuture<'_, ()> {
        self.inner.reload()
    }
}
