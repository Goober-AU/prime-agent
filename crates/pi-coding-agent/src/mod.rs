//! Port of packages/coding-agent/src/index.ts
//!
//! The TypeScript barrel re-exports 375 names from 38 modules. Every module it
//! re-exports from belongs to another slice, and several of those files are
//! still empty on disk, so this barrel publishes the names that exist today and
//! records the rest per source module, ready to uncomment as those slices land.
//!
//! A re-export of a name that does not exist is a compile error, so the missing
//! names stay commented rather than being re-exported speculatively.

// `export { ... } from "./config.js"`
pub use crate::config::{get_agent_dir, VERSION};
// `export { ... } from "./core/agent-session.js"`
pub use crate::core::agent_session::{
    AgentSession,
    AgentSessionConfig,
    AgentSessionEvent,
    AgentSessionEventListener,
    ModelCycleResult,
    PromptOptions,
};
// `export { ... } from "./core/auth-storage.js"`
pub use crate::core::auth_storage::{
    AuthCredential,
    AuthStatus,
    AuthStorage,
    AuthStorageBackend,
    FileAuthStorageBackend,
    InMemoryAuthStorageBackend,
};
// not yet ported (2): ApiKeyCredential, OAuthCredential
// `export { ... } from "./core/compaction/index.js"`
// not yet ported (20): BranchPreparation, BranchSummaryResult, CollectEntriesResult, CompactionResult, CutPointResult, calculateContextTokens, collectEntriesForBranchSummary, compact, DEFAULT_COMPACTION_SETTINGS, estimateTokens, FileOperations, findCutPoint, findTurnStartIndex, GenerateBranchSummaryOptions, generateBranchSummary, generateSummary, getLastAssistantUsage, prepareBranchEntries, serializeConversation, shouldCompact
// `export { ... } from "./core/event-bus.js"`
pub use crate::core::event_bus::{create_event_bus, EventBus, EventBusController};
// `export { ... } from "./core/extensions/builtin/memory.js"`
pub use crate::core::extensions::builtin::memory::{create_memory_extension};
// `export { ... } from "./core/extensions/index.js"`
pub use crate::core::extensions::{SlashCommandInfo, SlashCommandSource, SourceInfo};
// not yet ported (75): AgentEndEvent, AgentStartEvent, AgentToolResult, AgentToolUpdateCallback, AppKeybinding, AutocompleteProviderFactory, BashToolCallEvent, BeforeAgentStartEvent, BeforeAgentStartEventResult, BeforeProviderRequestEvent, BeforeProviderRequestEventResult, BuildSystemPromptOptions, CompactOptions, ContextEvent, ContextUsage, CustomToolCallEvent, EditToolCallEvent, ExecOptions, ExecResult, Extension, ExtensionActions, ExtensionAPI, ExtensionCommandContext, ExtensionCommandContextActions, ExtensionContext, ExtensionContextActions, ExtensionError, ExtensionEvent, ExtensionFactory, ExtensionFlag, ExtensionHandler, ExtensionRuntime, ExtensionShortcut, ExtensionUIContext, ExtensionUIDialogOptions, ExtensionWidgetOptions, InputEvent, InputEventResult, InputSource, IpythonToolCallEvent, KeybindingsManager, LoadExtensionsResult, MessageRenderer, MessageRenderOptions, ProviderConfig, ProviderModelConfig, RefineCompleteEvent, RefinePreparation, RegisteredCommand, RegisteredTool, ResolvedCommand, SessionBeforeCompactEvent, SessionBeforeForkEvent, SessionBeforeRefineEvent, SessionBeforeRefineResult, SessionBeforeSwitchEvent, SessionBeforeTreeEvent, SessionCompactEvent, SessionShutdownEvent, SessionStartEvent, SessionTreeEvent, TerminalInputHandler, ToolCallEvent, ToolCallEventResult, ToolDefinition, ToolExecutionMode, ToolInfo, ToolRenderResultOptions, ToolResultEvent, TurnEndEvent, TurnStartEvent, UserBashEvent, UserBashEventResult, WidgetPlacement, WorkingIndicatorOptions
// `export { ... } from "./core/extensions/index.js"`
pub use crate::core::extensions::{ExtensionRunner};
// not yet ported (9): createExtensionRuntime, defineTool, discoverAndLoadExtensions, isBashToolResult, isEditToolResult, isIpythonToolResult, isToolCallEventType, wrapRegisteredTool, wrapRegisteredTools
// `export { ... } from "./core/footer-data-provider.js"`
pub use crate::core::footer_data_provider::{ReadonlyFooterDataProvider};
// `export { ... } from "./core/memory/evidence.js"`
pub use crate::core::memory::evidence::{MemorySource};
// `export { ... } from "./core/memory/service.js"`
pub use crate::core::memory::service::{MemoryService};
// `export { ... } from "./core/memory/store.js"`
pub use crate::core::memory::store::{MemorySettings};
// `export { ... } from "./core/messages.js"`
pub use crate::core::messages::{convert_to_llm};
// `export { ... } from "./core/model-registry.js"`
pub use crate::core::model_registry::{ModelRegistry};
// `export { ... } from "./core/package-manager.js"`
pub use crate::core::package_manager::{
    PathMetadata,
    ProgressCallback,
    ProgressEvent,
    ResolvedPaths,
    ResolvedResource,
};
// not yet ported (1): PackageManager
// `export { ... } from "./core/package-manager.js"`
pub use crate::core::package_manager::{DefaultPackageManager};
// `export { ... } from "./core/performance-metrics.js"`
// not yet ported (6): createLocalPerformanceMetricRecorder, createLocalPerformanceMetricRecorderFromEnvironment, EnvironmentPerformanceMetricRecorderOptions, LocalPerformanceMetricRecorder, LocalPerformanceMetricRecorderOptions, PerformanceMetricFileIO
// `export { ... } from "./core/refinement/index.js"`
pub use crate::core::refinement::refinement::{
    HarnessState,
    RefinementEdit,
    RefinementProposal,
    RefinementResult,
};
// `export { ... } from "./core/resource-loader.js"`
pub use crate::core::resource_loader::{ResourceCollision, ResourceLoader};
// not yet ported (1): ResourceDiagnostic
// `export { ... } from "./core/resource-loader.js"`
pub use crate::core::resource_loader::{DefaultResourceLoader, load_project_context_files};
// `export { ... } from "./core/sdk.js"`
pub use crate::core::sdk::{
    AgentSessionRuntimeConfig,
    CreateAgentSessionOptions,
    CreateAgentSessionResult,
    create_bash_tool,
    create_ipython_tool,
};
// not yet re-exported by ./core/sdk.js (the module is owned by another slice and
// does not re-export these yet): AgentSessionRuntime, AgentSessionRuntimeMetadata,
// CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeResult,
// createAgentSessionFromServices, createAgentSessionRuntime,
// createAgentSessionServices
// not yet ported (10): AgentSessionCreationOptions, AgentSessionRuntimeDiagnostic, AgentSessionRuntimeKind, AgentSessionServices, CreateAgentSessionFromServicesOptions, CreateAgentSessionServicesOptions, CreateRlmSubagentRuntimeOptions, PromptTemplate, RlmSubagentRuntime, SubagentRuntimeHost
// `export { ... } from "./core/session-action-store.js"`
pub use crate::core::session_action_store::{SessionActionSnapshot};
// `export { ... } from "./core/session-import-errors.js"`
pub use crate::core::session_import_errors::{SessionImportFileNotFoundError};
// `export { ... } from "./core/session-manager.js"`
pub use crate::core::session_manager::{
    build_session_context,
    CURRENT_SESSION_VERSION,
    FileEntry,
    get_latest_compaction_entry,
    migrate_session_entries,
    NewSessionOptions,
    parse_session_entries,
    SessionContext,
    SessionEntry,
    SessionHeader,
    SessionInfo,
    SessionManager,
    SessionState,
    SessionStateStatus,
};
// not yet ported (10): BranchSummaryEntry, CompactionEntry, CustomEntry, CustomMessageEntry, ModelChangeEntry, SessionEntryBase, SessionInfoEntry, SessionMessageEntry, SessionStateEntry, ThinkingLevelChangeEntry
// `export { ... } from "./core/session-stats.js"`
pub use crate::core::session_stats::{SessionStats};
// `export { ... } from "./core/settings-manager.js"`
pub use crate::core::settings_manager::{
    CompactionSettings,
    ImageSettings,
    PackageSource,
    RetrySettings,
    SettingsManager,
};
// `export { ... } from "./core/skill-blocks.js"`
pub use crate::core::skill_blocks::{ParsedSkillBlock, parse_skill_block};
// `export { ... } from "./core/skills.js"`
pub use crate::core::skills::{
    format_skills_for_prompt,
    get_python_skill_runtime_info,
    LoadSkillsFromDirOptions,
    LoadSkillsResult,
    load_skills,
    load_skills_from_dir,
    MarkdownSkill,
    PythonSkill,
    PythonSkillRuntimeInfo,
    Skill,
    SkillFrontmatter,
    SkillKind,
    SkillPythonMetadata,
};
// `export { ... } from "./core/source-info.js"`
pub use crate::core::source_info::{create_synthetic_source_info};
// `export { ... } from "./core/tools/index.js"`
pub use crate::core::tools::{
    BashOperations,
    BashSpawnContext,
    BashSpawnHook,
    BashToolDetails,
    BashToolInput,
    BashToolOptions,
    create_bash_tool_definition,
    create_edit_tool_definition,
    create_ipython_tool_definition,
    create_local_bash_operations,
    DEFAULT_MAX_BYTES,
    DEFAULT_MAX_LINES,
    EditOperations,
    EditToolDetails,
    EditToolInput,
    EditToolOptions,
    format_size,
    IpythonKernelProvisioner,
    IpythonToolDetails,
    IpythonToolInput,
    IpythonToolOptions,
    ToolsOptions,
    TruncationOptions,
    TruncationResult,
    truncate_head,
    truncate_line,
    truncate_tail,
    with_file_mutation_queue,
};
// `export { ... } from "./main.js"`
// blocked_on: ./main.js is this slice's main_entry.rs (main() is not a name here).
// not yet ported (2): MainOptions, main
// `export { ... } from "./modes/agent-connection/index.js"`
pub use crate::modes::agent_connection::{
    AgentConnection,
    AgentConnectionArtifactReference,
    AgentConnectionArtifactType,
    AgentConnectionEvent,
    AgentConnectionExtensionUiRequest,
    AgentConnectionExtensionUiResponse,
    AgentConnectionModel,
    AgentConnectionModelCycleResult,
    AgentConnectionQueueState,
    AgentConnectionResourceSnapshot,
    AgentConnectionRlmChildAgentSnapshot,
    AgentConnectionSessionEvent,
    AgentConnectionSlashCommand,
    AgentConnectionState,
    DaemonAgentConnection,
    InProcessAgentConnection,
};
// `export { ... } from "./modes/index.js"`
// not yet ported (51): ClientPromptStashStore, createInteractiveModeLocalSessionHost, createInteractiveModeUiServices, createInteractiveModeUiServicesFromServices, DAEMON_PROTOCOL_INFO, DAEMON_PROTOCOL_NAME, DAEMON_PROTOCOL_VERSION, DaemonArtifactReference, DaemonAttachResult, DaemonClient, DaemonClientCapability, DaemonClientId, DaemonClientMessageListener, DaemonCommand, DaemonCommandEnvelope, DaemonCommandId, DaemonEventEnvelope, DaemonEventId, DaemonEventMeta, DaemonEventSequence, DaemonModeOptions, DaemonOutbound, DaemonProtocolInfo, DaemonProtocolName, DaemonProtocolVersion, DaemonReplayInfo, DaemonReplayStatus, DaemonResponse, DaemonResumeCursor, DaemonSessionSnapshot, defaultDaemonSocketPath, InteractiveInitialPrompt, InteractiveMode, InteractiveModeLocalSessionHost, InteractiveModeOptions, InteractiveModeUiServices, ModelInfo, PrintModeOptions, PromptStash, PromptStashState, RpcClient, RpcClientOptions, RpcCommand, RpcEventListener, RpcResponse, RpcSessionState, runPrintMode, runRpcMode, SessionActivity, SessionLifecycle, SessionSummary
// `export { ... } from "./modes/interactive/components/index.js"`
pub use crate::modes::interactive::components::{
    ArminComponent,
    ExtensionEditorComponent,
    ThinkingSelectorComponent,
    TreeSelectorComponent,
};
// not yet ported (34): AgentMessageComponent, AssistantMessageComponent, BashExecutionComponent, BorderedLoader, BranchSummaryMessageComponent, CompactionSummaryMessageComponent, ConfigurationMenuComponent, ConfigurationMenuTab, CustomEditor, CustomMessageComponent, DynamicBorder, ExtensionInputComponent, ExtensionSelectorComponent, FooterComponent, keyHint, keyText, LoginDialogComponent, ModelSelectorComponent, OAuthSelectorComponent, RenderDiffOptions, rawKeyHint, renderDiff, SettingsCallbacks, SettingsConfig, SettingsSelectorComponent, ShowImagesSelectorComponent, SkillInvocationMessageComponent, ThemeSelectorComponent, ToolExecutionComponent, ToolExecutionOptions, truncateToVisualLines, UserMessageComponent, UserMessageSelectorComponent, VisualTruncateResult
// `export { ... } from "./modes/interactive/theme/theme.js"`
pub use crate::modes::interactive::theme::theme::{
    get_language_from_path,
    get_markdown_theme,
    get_select_list_theme,
    get_settings_list_theme,
    highlight_code,
    init_theme,
    Theme,
    ThemeColor,
};
// `export { ... } from "./utils/clipboard.js"`
pub use crate::utils::clipboard::{copy_to_clipboard};
// `export { ... } from "./utils/frontmatter.js"`
pub use crate::utils::frontmatter::{parse_frontmatter, strip_frontmatter};
// `export { ... } from "./utils/shell.js"`
pub use crate::utils::shell::{get_shell_config};
