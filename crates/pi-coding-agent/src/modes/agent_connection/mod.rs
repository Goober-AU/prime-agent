//! Port of packages/coding-agent/src/modes/agent-connection/index.ts

pub mod daemon_agent_connection;
pub mod in_process_agent_connection;
pub mod snapshot;
pub mod tool_definition;
pub mod types;

pub use daemon_agent_connection::DaemonAgentConnection;
pub use in_process_agent_connection::InProcessAgentConnection;
pub use snapshot::{create_agent_connection_commands, create_agent_connection_state};
pub use types::AgentConnectionPromptAdmissionError;

pub use types::{
    AgentConnection, AgentConnectionAgentStatus, AgentConnectionArtifactReference, AgentConnectionArtifactType,
    AgentConnectionBeforeSessionInvalidateListener, AgentConnectionBranchSummaryEntry,
    AgentConnectionChildUsageAttributionEntry, AgentConnectionCompactionEntry, AgentConnectionCustomEntry,
    AgentConnectionCustomMessageEntry, AgentConnectionEvent, AgentConnectionEventListener,
    AgentConnectionExtensionUiRequest, AgentConnectionExtensionUiResponse, AgentConnectionForkOptions,
    AgentConnectionHeartbeat, AgentConnectionHistoryRange, AgentConnectionHistoryRangeRequest,
    AgentConnectionHistoryWindow, AgentConnectionLabelEntry, AgentConnectionModel, AgentConnectionModelCatalog,
    AgentConnectionModelChangeEntry, AgentConnectionModelCycleResult, AgentConnectionNavigateTreeOptions,
    AgentConnectionNavigateTreeResult, AgentConnectionNewSessionOptions, AgentConnectionParentMetadata,
    AgentConnectionPromptOptions, AgentConnectionQueuedMessageLane, AgentConnectionQueuedMessageMutation,
    AgentConnectionQueuedMessageMutationStatus, AgentConnectionQueueMode, AgentConnectionQueueState,
    AgentConnectionReplayInfo, AgentConnectionReplayStatus, AgentConnectionResourceCollision,
    AgentConnectionResourceContextFile, AgentConnectionResourceDiagnostic, AgentConnectionResourceDiagnostics,
    AgentConnectionResourceExtension, AgentConnectionResourcePrompt, AgentConnectionResourceSkill,
    AgentConnectionResourceSnapshot, AgentConnectionResourceTheme, AgentConnectionRlmChildAgentActivity,
    AgentConnectionRlmChildAgentSnapshot, AgentConnectionRlmChildAgentStatus, AgentConnectionSavedSessionInfo,
    AgentConnectionSavedSessionScope, AgentConnectionSavedSessionState, AgentConnectionSavedSessionStateStatus,
    AgentConnectionScopedModel, AgentConnectionServiceTierChangeEntry, AgentConnectionSessionContext,
    AgentConnectionSessionEntry, AgentConnectionSessionEntryBase, AgentConnectionSessionEvent,
    AgentConnectionSessionHeader, AgentConnectionSessionInfoEntry, AgentConnectionSessionInputPause,
    AgentConnectionSessionListCallbacks, AgentConnectionSessionListProgress, AgentConnectionSessionMessageEntry,
    AgentConnectionSessionStateEntry, AgentConnectionSessionTreeFlatNode, AgentConnectionSessionTreeNode,
    AgentConnectionSessionWatcher, AgentConnectionSideQuestionEvent, AgentConnectionSideQuestionTurn,
    AgentConnectionSlashCommand, AgentConnectionSnapshot, AgentConnectionSourceInfo, AgentConnectionSourceOrigin,
    AgentConnectionSourceScope, AgentConnectionState, AgentConnectionSwitchSessionOptions,
    AgentConnectionThinkingLevelChangeEntry, AgentConnectionToolDefinition, AgentConnectionUserMessage,
};
