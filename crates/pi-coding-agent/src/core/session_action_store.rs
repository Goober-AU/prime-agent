//! Port of packages/coding-agent/src/core/session-action-store.ts

#![allow(clippy::too_many_arguments)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pi_ai::types::{ImageContent, UserMessage};
use tokio::sync::oneshot;
use futures::future::{BoxFuture, FutureExt, Shared};
use serde::{Deserialize, Serialize};

pub use crate::core::extensions::types::InputSource;
pub use crate::core::messages::CustomMessage;
pub use crate::core::slash_commands::SessionSlashCommand;

/// `InputSource | "internal"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionSource {
    Input(InputSource),
    Internal,
}

impl ActionSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionSource::Input(source) => source.as_str(),
            ActionSource::Internal => "internal",
        }
    }
}

impl Serialize for ActionSource {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ActionSource {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match String::deserialize(deserializer)?.as_str() {
            "interactive" => Ok(Self::Input(InputSource::Interactive)),
            "rpc" => Ok(Self::Input(InputSource::Rpc)),
            "extension" => Ok(Self::Input(InputSource::Extension)),
            "internal" => Ok(Self::Internal),
            value => Err(serde::de::Error::unknown_variant(value, &["interactive", "rpc", "extension", "internal"])),
        }
    }
}

/// `DeliveryRecord["message"]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DeliveryMessage {
    Custom(CustomMessage),
    User(UserMessage),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryPolicy {
    NextTurnBoundary,
    WhenRunIdle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActionPriority { Background, User, Pinned }

impl DeliveryPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeliveryPolicy::NextTurnBoundary => "next_turn_boundary",
            DeliveryPolicy::WhenRunIdle => "when_run_idle",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakePolicy {
    Immediate,
    OnLowerBoundary,
    ExternalResume,
}

impl WakePolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            WakePolicy::Immediate => "immediate",
            WakePolicy::OnLowerBoundary => "on_lower_boundary",
            WakePolicy::ExternalResume => "external_resume",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueuedMessageLane {
    Steering,
    FollowUp,
}

impl QueuedMessageLane {
    pub fn as_str(&self) -> &'static str {
        match self {
            QueuedMessageLane::Steering => "steering",
            QueuedMessageLane::FollowUp => "followUp",
        }
    }
}

pub fn queued_message_lane_delivery_policy(lane: QueuedMessageLane) -> DeliveryPolicy {
    if lane == QueuedMessageLane::Steering {
        DeliveryPolicy::NextTurnBoundary
    } else {
        DeliveryPolicy::WhenRunIdle
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QueuedMessageMutation {
    Delete,
    Move {
        /// `-1 | 1`
        direction: i8,
    },
    Replace {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
        lane: QueuedMessageLane,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuedMessageMutationStatus {
    Applied,
    Rejected,
    Invalid,
}

impl QueuedMessageMutationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            QueuedMessageMutationStatus::Applied => "applied",
            QueuedMessageMutationStatus::Rejected => "rejected",
            QueuedMessageMutationStatus::Invalid => "invalid",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionActionSnapshot {
    pub queued_count: i64,
    pub steering: Vec<String>,
    pub follow_ups: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<SessionActionSnapshotActive>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionActionSnapshotActive {
    pub kind: SessionActionSnapshotKind,
    pub phase: SessionActionPhase,
    /// `label?`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActionSnapshotKind {
    Turn,
    SessionCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActionPhase {
    Preparing,
    Committing,
    Running,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryRecord {
    pub id: String,
    pub role: DeliveryRecordRole,
    pub message: DeliveryMessage,
    pub started: bool,
    pub durable: bool,
    pub owner_action_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryRecordRole {
    Primary,
    Prefix,
    NextTurn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTurnPayload {
    pub records: Vec<DeliveryRecord>,
    pub text: String,
    /// `preview?`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCommandPayload {
    pub command: SessionSlashCommand,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionActionPayload {
    Turn(SessionTurnPayload),
    SessionCommand(SessionCommandPayload),
}

impl SessionActionPayload {
    /// `payload.kind`
    pub fn kind(&self) -> &'static str {
        match self {
            SessionActionPayload::Turn(_) => "turn",
            SessionActionPayload::SessionCommand(_) => "session_command",
        }
    }
}

/// `ActionLifecycle` - the `state` discriminant plus per-state fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ActionLifecycle {
    Queued,
    Selected,
    Preparing {
        /// `preparation?: object`
        #[serde(skip_serializing_if = "Option::is_none")]
        preparation: Option<serde_json::Value>,
    },
    Committing,
    Running {
        execution: ActionExecution,
    },
    Completed,
    Failed {
        error: String,
    },
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionExecution {
    AgentTurn,
    SessionCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionLifecycleState {
    Queued,
    Selected,
    Preparing,
    Committing,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl ActionLifecycle {
    pub fn state(&self) -> ActionLifecycleState {
        match self {
            ActionLifecycle::Queued => ActionLifecycleState::Queued,
            ActionLifecycle::Selected => ActionLifecycleState::Selected,
            ActionLifecycle::Preparing { .. } => ActionLifecycleState::Preparing,
            ActionLifecycle::Committing => ActionLifecycleState::Committing,
            ActionLifecycle::Running { .. } => ActionLifecycleState::Running,
            ActionLifecycle::Completed => ActionLifecycleState::Completed,
            ActionLifecycle::Failed { .. } => ActionLifecycleState::Failed,
            ActionLifecycle::Cancelled => ActionLifecycleState::Cancelled,
        }
    }
}

impl ActionLifecycleState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionLifecycleState::Queued => "queued",
            ActionLifecycleState::Selected => "selected",
            ActionLifecycleState::Preparing => "preparing",
            ActionLifecycleState::Committing => "committing",
            ActionLifecycleState::Running => "running",
            ActionLifecycleState::Completed => "completed",
            ActionLifecycleState::Failed => "failed",
            ActionLifecycleState::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAction<TPayload = SessionActionPayload> {
    pub id: String,
    pub source: ActionSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<SessionActionPriority>,
    pub delivery: DeliveryPolicy,
    pub wake: WakePolicy,
    pub payload: TPayload,
    pub lifecycle: ActionLifecycle,
    /// `queueKey?`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
    /// `agentMessageId?`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_message_id: Option<String>,
    /// `suppressAutonomousContinuation?`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suppress_autonomous_continuation: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RollbackProof {
    /// `dispatchSettled: true`
    pub dispatch_settled: bool,
    pub transcript: Vec<pi_agent_core::types::AgentMessage>,
}

const TERMINAL_STATES: [ActionLifecycleState; 3] = [
    ActionLifecycleState::Completed,
    ActionLifecycleState::Failed,
    ActionLifecycleState::Cancelled,
];
const ACTIVE_STATES: [ActionLifecycleState; 4] = [
    ActionLifecycleState::Selected,
    ActionLifecycleState::Preparing,
    ActionLifecycleState::Committing,
    ActionLifecycleState::Running,
];
const CLEARABLE_STATES: [ActionLifecycleState; 3] = [
    ActionLifecycleState::Queued,
    ActionLifecycleState::Selected,
    ActionLifecycleState::Preparing,
];

fn is_terminal(state: ActionLifecycleState) -> bool {
    TERMINAL_STATES.contains(&state)
}

fn is_active(state: ActionLifecycleState) -> bool {
    ACTIVE_STATES.contains(&state)
}

fn is_clearable<TPayload>(action: &SessionAction<TPayload>) -> bool {
    CLEARABLE_STATES.contains(&action.lifecycle.state())
}

fn legal_transitions(state: ActionLifecycleState) -> &'static [ActionLifecycleState] {
    match state {
        ActionLifecycleState::Queued => &[
            ActionLifecycleState::Selected,
            ActionLifecycleState::Failed,
            ActionLifecycleState::Cancelled,
        ],
        ActionLifecycleState::Selected => &[
            ActionLifecycleState::Queued,
            ActionLifecycleState::Preparing,
            ActionLifecycleState::Running,
            ActionLifecycleState::Failed,
            ActionLifecycleState::Cancelled,
        ],
        ActionLifecycleState::Preparing => &[
            ActionLifecycleState::Queued,
            ActionLifecycleState::Committing,
            ActionLifecycleState::Failed,
            ActionLifecycleState::Cancelled,
        ],
        ActionLifecycleState::Committing => &[
            ActionLifecycleState::Queued,
            ActionLifecycleState::Running,
            ActionLifecycleState::Failed,
            ActionLifecycleState::Cancelled,
        ],
        ActionLifecycleState::Running => &[
            ActionLifecycleState::Completed,
            ActionLifecycleState::Failed,
            ActionLifecycleState::Cancelled,
        ],
        ActionLifecycleState::Completed
        | ActionLifecycleState::Failed
        | ActionLifecycleState::Cancelled => &[],
    }
}

/// The shared fields required by TypeScript's `TPayload extends SessionActionPayload`.
pub trait SessionPayload {
    fn records(&self) -> &[DeliveryRecord];
    fn preview(&self) -> &str;
}

impl<TPayload: SessionPayload> SessionAction<TPayload> {
    pub fn effective_priority(&self) -> SessionActionPriority {
        self.priority.unwrap_or_else(|| {
            let human = matches!(self.source, ActionSource::Input(InputSource::Interactive | InputSource::Rpc));
            let agent_traffic = self.agent_message_id.as_deref().is_some_and(|id| id.starts_with("agentmsg_"));
            let custom = self.payload.records().iter().find(|record| record.role == DeliveryRecordRole::Primary)
                .is_some_and(|record| matches!(record.message, DeliveryMessage::Custom(_)));
            if human && !agent_traffic && !custom { SessionActionPriority::User } else { SessionActionPriority::Background }
        })
    }
}

impl SessionPayload for SessionActionPayload {
    fn records(&self) -> &[DeliveryRecord] {
        match self {
            Self::Turn(turn) => &turn.records,
            Self::SessionCommand(_) => &[],
        }
    }

    fn preview(&self) -> &str {
        match self {
            Self::Turn(turn) => turn.preview.as_deref().unwrap_or(&turn.text),
            Self::SessionCommand(command) => &command.text,
        }
    }
}

fn primary_records<TPayload: SessionPayload>(action: &SessionAction<TPayload>) -> Vec<&DeliveryRecord> {
    action.payload.records().iter()
        .filter(|record| record.role == DeliveryRecordRole::Primary)
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct TransitionOptions {
    pub rollback_proof: Option<RollbackProof>,
}

pub fn transition_session_action<TPayload: SessionPayload>(
    action: &mut SessionAction<TPayload>,
    next: ActionLifecycle,
    options: &TransitionOptions,
) -> Result<(), String> {
    let previous = action.lifecycle.state();
    let next_state = next.state();
    if !legal_transitions(previous).contains(&next_state) {
        return Err(format!(
            "Illegal session action lifecycle transition: {} -> {}",
            previous.as_str(),
            next_state.as_str()
        ));
    }
    if previous == ActionLifecycleState::Committing && next_state == ActionLifecycleState::Queued {
        let settled = options
            .rollback_proof
            .as_ref()
            .map(|proof| proof.dispatch_settled)
            .unwrap_or(false);
        if !settled {
            return Err(
                "Committing session action rollback requires a settled dispatch and transcript proof"
                    .to_string(),
            );
        }
        let transcript: Vec<&pi_agent_core::types::AgentMessage> = options
            .rollback_proof
            .as_ref()
            .map(|proof| proof.transcript.iter().collect())
            .unwrap_or_default();
        let durable = primary_records(action)
            .iter()
            .any(|record| match &record.message {
                DeliveryMessage::User(message) => transcript.iter().any(|candidate| {
                    matches!(
                        candidate,
                        pi_agent_core::types::AgentMessage::Message(pi_ai::types::Message::User(user))
                            if user == message
                    )
                }),
                DeliveryMessage::Custom(message) => transcript.iter().any(|candidate| {
                    **candidate == crate::core::messages::custom_message_to_agent_message(message.clone())
                }),
            });
        if durable {
            return Err(
                "Cannot roll back a session action whose primary message is durable in the transcript"
                    .to_string(),
            );
        }
    }
    action.lifecycle = next;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionDisposition {
    StartsWhenAdmitted,
    Queued,
}

impl AdmissionDisposition {
    pub fn as_str(&self) -> &'static str {
        match self {
            AdmissionDisposition::StartsWhenAdmitted => "starts_when_admitted",
            AdmissionDisposition::Queued => "queued",
        }
    }
}

#[derive(Debug, Clone)]
pub enum SubmissionOutcome {
    Accepted {
        action_id: String,
        disposition: AdmissionDisposition,
    },
    Coalesced {
        existing_action_id: String,
    },
    HandledWithoutTurn,
    /// `completion: Promise<void>` - the completion is signalled by the sender.
    ExtensionCommand {
        completion: Arc<tokio::sync::Notify>,
    },
}

/// `Notify` has no `PartialEq`, so the discriminant and payloads are compared
/// explicitly; two extension commands are equal when they share one notifier.
impl PartialEq for SubmissionOutcome {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                SubmissionOutcome::Accepted {
                    action_id: left_id,
                    disposition: left,
                },
                SubmissionOutcome::Accepted {
                    action_id: right_id,
                    disposition: right,
                },
            ) => left_id == right_id && left == right,
            (
                SubmissionOutcome::Coalesced {
                    existing_action_id: left,
                },
                SubmissionOutcome::Coalesced {
                    existing_action_id: right,
                },
            ) => left == right,
            (SubmissionOutcome::HandledWithoutTurn, SubmissionOutcome::HandledWithoutTurn) => true,
            (
                SubmissionOutcome::ExtensionCommand { completion: left },
                SubmissionOutcome::ExtensionCommand { completion: right },
            ) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeliveryOutcome {
    Delivered,
    NotApplicable,
}

/// Cloneable promises preserve repeated awaiting and retain the original rejection.
#[derive(Clone)]
pub struct ActionTicket {
    pub id: String,
    pub accepted: Shared<BoxFuture<'static, Result<SubmissionOutcome, String>>>,
    pub delivered: Shared<BoxFuture<'static, Result<DeliveryOutcome, String>>>,
    pub completed: Shared<BoxFuture<'static, Result<(), String>>>,
}

struct Deferred<T> {
    sender: Option<oneshot::Sender<Result<T, String>>>,
    settled: bool,
    rejected: Option<String>,
}

impl<T> Deferred<T> {
    fn settle(&mut self, value: T) -> bool {
        if self.settled { return false; }
        self.settled = true;
        if let Some(sender) = self.sender.take() { let _ = sender.send(Ok(value)); }
        true
    }

    fn reject(&mut self, error: String) -> bool {
        if self.settled { return false; }
        self.settled = true;
        self.rejected = Some(error.clone());
        if let Some(sender) = self.sender.take() { let _ = sender.send(Err(error)); }
        true
    }
}

fn deferred_promise<T: Clone + Send + Sync + 'static>(receiver: oneshot::Receiver<Result<T, String>>)
    -> Shared<BoxFuture<'static, Result<T, String>>>
{
    async move { receiver.await.unwrap_or_else(|_| Err("Session action ticket was dropped before settlement".to_string())) }
        .boxed().shared()
}

pub struct ActionTicketController {
    pub ticket: ActionTicket,
    accepted: Mutex<Deferred<SubmissionOutcome>>,
    delivered: Mutex<Deferred<DeliveryOutcome>>,
    completed: Mutex<Deferred<()>>,
    completed_error: Mutex<Option<String>>,
}

impl ActionTicketController {
    pub fn new(id: &str) -> Self {
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let (delivered_tx, delivered_rx) = oneshot::channel();
        let (completed_tx, completed_rx) = oneshot::channel();
        ActionTicketController {
            ticket: ActionTicket {
                id: id.to_string(),
                accepted: deferred_promise(accepted_rx),
                delivered: deferred_promise(delivered_rx),
                completed: deferred_promise(completed_rx),
            },
            accepted: Mutex::new(Deferred {
                sender: Some(accepted_tx),
                settled: false,
                rejected: None,
            }),
            delivered: Mutex::new(Deferred {
                sender: Some(delivered_tx),
                settled: false,
                rejected: None,
            }),
            completed: Mutex::new(Deferred {
                sender: Some(completed_tx),
                settled: false,
                rejected: None,
            }),
            completed_error: Mutex::new(None),
        }
    }

    pub fn settle_accepted(&self, outcome: SubmissionOutcome) -> bool {
        self.accepted
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .settle(outcome)
    }

    pub fn settle_delivered(&self, outcome: DeliveryOutcome) -> bool {
        self.delivered
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .settle(outcome)
    }

    pub fn reject_delivered(&self, error: String) -> bool {
        self.delivered
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .reject(error)
    }

    /// `settleCompleted(error?)` - the error is stored for the awaiting side.
    pub fn settle_completed(&self, error: Option<String>) -> bool {
        let mut completed = self.completed.lock().unwrap_or_else(|lock| lock.into_inner());
        match error {
            Some(error) => {
                let settled = completed.reject(error.clone());
                if settled {
                    *self.completed_error.lock().unwrap_or_else(|lock| lock.into_inner()) = Some(error);
                }
                settled
            }
            None => completed.settle(()),
        }
    }

    pub fn completed_error(&self) -> Option<String> {
        self.completed_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

pub struct ActionStore<TAction: Clone = SessionAction> {
    next_turn_boundary: Vec<TAction>,
    when_run_idle: Vec<TAction>,
    tickets: HashMap<String, Arc<ActionTicketController>>,
    id_of: Arc<dyn Fn(&TAction) -> String + Send + Sync>,
    delivery_of: Arc<dyn Fn(&TAction) -> DeliveryPolicy + Send + Sync>,
    state_of: Arc<dyn Fn(&TAction) -> ActionLifecycleState + Send + Sync>,
    priority_of: Arc<dyn Fn(&TAction) -> SessionActionPriority + Send + Sync>,
    set_delivery: Arc<dyn Fn(&mut TAction, DeliveryPolicy) + Send + Sync>,
    transition: Arc<dyn Fn(&mut TAction, ActionLifecycle) -> Result<(), String> + Send + Sync>,
}

impl<TAction: Clone> ActionStore<TAction> {
    /// `TAction extends SessionAction`: the accessors make the store usable with
    /// any action shape while keeping one implementation.
    fn with_accessors(
        id_of: Arc<dyn Fn(&TAction) -> String + Send + Sync>,
        delivery_of: Arc<dyn Fn(&TAction) -> DeliveryPolicy + Send + Sync>,
        state_of: Arc<dyn Fn(&TAction) -> ActionLifecycleState + Send + Sync>,
        priority_of: Arc<dyn Fn(&TAction) -> SessionActionPriority + Send + Sync>,
        set_delivery: Arc<dyn Fn(&mut TAction, DeliveryPolicy) + Send + Sync>,
        transition: Arc<dyn Fn(&mut TAction, ActionLifecycle) -> Result<(), String> + Send + Sync>,
    ) -> Self {
        ActionStore {
            next_turn_boundary: Vec::new(),
            when_run_idle: Vec::new(),
            tickets: HashMap::new(),
            id_of,
            delivery_of,
            state_of,
            priority_of,
            set_delivery,
            transition,
        }
    }

    fn id(&self, action: &TAction) -> String {
        (self.id_of)(action)
    }

    fn delivery(&self, action: &TAction) -> DeliveryPolicy {
        (self.delivery_of)(action)
    }

    fn state(&self, action: &TAction) -> ActionLifecycleState {
        (self.state_of)(action)
    }

    fn list(&mut self, policy: DeliveryPolicy) -> &mut Vec<TAction> {
        match policy {
            DeliveryPolicy::NextTurnBoundary => &mut self.next_turn_boundary,
            DeliveryPolicy::WhenRunIdle => &mut self.when_run_idle,
        }
    }

    fn list_ref(&self, policy: DeliveryPolicy) -> &Vec<TAction> {
        match policy {
            DeliveryPolicy::NextTurnBoundary => &self.next_turn_boundary,
            DeliveryPolicy::WhenRunIdle => &self.when_run_idle,
        }
    }

    fn assert_new_action(&self, action: &TAction) -> Result<(), String> {
        if self.state(action) != ActionLifecycleState::Queued {
            return Err("Only queued session actions can be enqueued".to_string());
        }
        if self.tickets.contains_key(&self.id(action)) {
            return Err(format!("Duplicate session action id: {}", self.id(action)));
        }
        Ok(())
    }

    pub fn enqueue(&mut self, action: TAction) -> Result<(), String> {
        self.assert_new_action(&action)?;
        let delivery = self.delivery(&action);
        let id = self.id(&action);
        let priority = (self.priority_of)(&action);
        let priority_of = self.priority_of.clone();
        let state_of = self.state_of.clone();
        let list = self.list(delivery);
        // Only overtake the trailing lower-priority queued work. Never jump
        // ahead of an already selected action or reorder same-priority input.
        let mut index = list.len();
        for (position, item) in list.iter().enumerate().rev() {
            if state_of(item) != ActionLifecycleState::Queued || priority_of(item) >= priority {
                break;
            }
            index = position;
        }
        list.insert(index, action);
        self.tickets.insert(id.clone(), Arc::new(ActionTicketController::new(&id)));
        Ok(())
    }

    /// Recovery replays its saved order rather than reprioritizing old input.
    pub fn enqueue_tail(&mut self, action: TAction) -> Result<(), String> {
        self.assert_new_action(&action)?;
        let delivery = self.delivery(&action);
        let id = self.id(&action);
        self.list(delivery).push(action);
        self.tickets
            .insert(id.clone(), Arc::new(ActionTicketController::new(&id)));
        Ok(())
    }

    pub fn enqueue_front(&mut self, action: TAction) -> Result<(), String> {
        self.assert_new_action(&action)?;
        let delivery = self.delivery(&action);
        let id = self.id(&action);
        let state_of = Arc::clone(&self.state_of);
        let list = self.list(delivery);
        let first_queued = list
            .iter()
            .position(|item| state_of(item) == ActionLifecycleState::Queued);
        let index = first_queued.unwrap_or(list.len());
        list.insert(index, action);
        self.tickets
            .insert(id.clone(), Arc::new(ActionTicketController::new(&id)));
        Ok(())
    }

    pub fn select_first(&mut self) -> Result<Option<TAction>, String> {
        let transition = Arc::clone(&self.transition);
        let state_of = Arc::clone(&self.state_of);
        let found_index = {
            let next_turn_index = self
                .next_turn_boundary
                .iter()
                .position(|item| state_of(item) == ActionLifecycleState::Queued);
            match next_turn_index {
                Some(index) => Some((DeliveryPolicy::NextTurnBoundary, index)),
                None => self
                    .when_run_idle
                    .iter()
                    .position(|item| state_of(item) == ActionLifecycleState::Queued)
                    .map(|index| (DeliveryPolicy::WhenRunIdle, index)),
            }
        };
        let Some((policy, index)) = found_index else {
            return Ok(None);
        };
        let list = self.list(policy);
        if let Some(action) = list.get_mut(index) {
            transition(action, ActionLifecycle::Selected)?;
            return Ok(Some(action.clone()));
        }
        Ok(None)
    }

    pub fn remove(
        &mut self,
        predicate: &dyn Fn(&TAction) -> bool,
        candidates: Option<&[TAction]>,
    ) -> Result<Vec<TAction>, String> {
        let candidates = candidates.map(<[TAction]>::to_vec)
            .unwrap_or_else(|| self.clearable_actions(None));
        let mut removed = Vec::new();
        for candidate in candidates.into_iter().filter(|action| predicate(action)) {
            let id = self.id(&candidate);
            let transition = Arc::clone(&self.transition);
            let id_of = Arc::clone(&self.id_of);
            let action = self.next_turn_boundary.iter_mut()
                .chain(self.when_run_idle.iter_mut())
                .find(|action| id_of(action) == id)
                .ok_or_else(|| format!("Session action {id} is not owned by this store"))?;
            transition(action, ActionLifecycle::Cancelled)?;
            removed.push(action.clone());
        }
        Ok(removed)
    }

    /// `rollback(action, proof?)` - the proof is checked by `transition`.
    pub fn rollback_with(
        &mut self,
        action: &mut TAction,
        proof: Option<RollbackProof>,
        transition_with_proof: &dyn Fn(
            &mut TAction,
            ActionLifecycle,
            Option<RollbackProof>,
        ) -> Result<(), String>,
    ) -> Result<(), String> {
        let _ = &self.transition;
        transition_with_proof(action, ActionLifecycle::Queued, proof)
    }

    pub fn swap_queued(&mut self, left: &TAction, right: &TAction) -> Result<(), String> {
        if self.state(left) != ActionLifecycleState::Queued
            || self.state(right) != ActionLifecycleState::Queued
            || self.delivery(left) != self.delivery(right)
        {
            return Err("Only queued actions in the same lane can be swapped".to_string());
        }
        let delivery = self.delivery(left);
        let left_id = self.id(left);
        let right_id = self.id(right);
        let id_of = Arc::clone(&self.id_of);
        let list = self.list(delivery);
        let left_index = list.iter().position(|item| id_of(item) == left_id);
        let right_index = list.iter().position(|item| id_of(item) == right_id);
        let (left_index, right_index) = match (left_index, right_index) {
            (Some(left_index), Some(right_index)) => (left_index, right_index),
            _ => return Err("Queued action is not owned by this store".to_string()),
        };
        list.swap(left_index, right_index);
        Ok(())
    }

    pub fn move_queued(
        &mut self,
        action: &mut TAction,
        delivery: DeliveryPolicy,
        index: usize,
    ) -> Result<(), String> {
        if self.state(action) != ActionLifecycleState::Queued {
            return Err("Only queued actions can be moved".to_string());
        }
        let id = self.id(action);
        let source_policy = self.delivery(action);
        let id_of = Arc::clone(&self.id_of);
        let source_index = self
            .list_ref(source_policy)
            .iter()
            .position(|item| id_of(item) == id);
        let source_index = match source_index {
            Some(source_index) => source_index,
            None => return Err(format!("Session action {id} is not owned by this store")),
        };
        self.list(source_policy).remove(source_index);
        (self.set_delivery)(action, delivery);
        let state_of = Arc::clone(&self.state_of);
        let target = self.list(delivery);
        let queued: Vec<String> = target
            .iter()
            .filter(|item| state_of(item) == ActionLifecycleState::Queued)
            .map(|item| id_of(item))
            .collect();
        let clamped = index.min(queued.len());
        let before_id = queued.get(clamped).cloned();
        let insert_at = match before_id {
            Some(before_id) => target
                .iter()
                .position(|item| id_of(item) == before_id)
                .unwrap_or(target.len()),
            None => target.len(),
        };
        target.insert(insert_at, action.clone());
        Ok(())
    }

    fn actions(&self, policy: Option<DeliveryPolicy>) -> Vec<TAction> {
        match policy {
            Some(policy) => self.list_ref(policy).clone(),
            None => {
                let mut all = self.next_turn_boundary.clone();
                all.extend(self.when_run_idle.iter().cloned());
                all
            }
        }
    }

    pub fn queued_actions(&self, policy: Option<DeliveryPolicy>) -> Vec<TAction> {
        self.actions(policy)
            .into_iter()
            .filter(|action| self.state(action) == ActionLifecycleState::Queued)
            .collect()
    }

    pub fn clearable_actions(&self, policy: Option<DeliveryPolicy>) -> Vec<TAction> {
        let state_of = Arc::clone(&self.state_of);
        self.actions(policy)
            .into_iter()
            .filter(|action| CLEARABLE_STATES.contains(&state_of(action)))
            .collect()
    }

    pub fn snapshot_actions(&self) -> Vec<TAction> {
        self.queued_actions(None)
    }

    pub fn unfinished_actions(&self, policy: Option<DeliveryPolicy>) -> Vec<TAction> {
        let state_of = Arc::clone(&self.state_of);
        self.actions(policy)
            .into_iter()
            .filter(|action| !is_terminal(state_of(action)))
            .collect()
    }

    pub fn active_actions(&self, policy: Option<DeliveryPolicy>) -> Vec<TAction> {
        let state_of = Arc::clone(&self.state_of);
        self.actions(policy)
            .into_iter()
            .filter(|action| is_active(state_of(action)))
            .collect()
    }

    /// The caller supplies the preview projection because the payload shape is
    /// owned by the action type (`turn.preview ?? turn.text` or `command.text`).
    fn queue_preview_with(
        &self,
        policy: DeliveryPolicy,
        preview_of: &dyn Fn(&TAction) -> String,
    ) -> Vec<String> {
        self.queued_actions(Some(policy))
            .into_iter()
            .map(|action| preview_of(&action))
            .collect()
    }

    pub fn ticket_for(&self, action: &TAction) -> Result<Arc<ActionTicketController>, String> {
        let id = self.id(action);
        self.tickets
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("Session action {id} is not owned by this store"))
    }

    pub fn owned_actions(&self) -> Vec<TAction> {
        self.actions(None)
    }

    pub fn release_terminal(&mut self, action: &TAction) -> Result<(), String> {
        if !is_terminal(self.state(action)) {
            return Err(format!(
                "Cannot release nonterminal session action {}",
                self.id(action)
            ));
        }
        let delivery = self.delivery(action);
        let id = self.id(action);
        let id_of = Arc::clone(&self.id_of);
        let list = self.list(delivery);
        if let Some(index) = list.iter().position(|item| id_of(item) == id) {
            list.remove(index);
        }
        self.tickets.remove(&id);
        Ok(())
    }
}

/// The default `SessionAction` specialization of the TypeScript store.
pub type SessionActionStore = ActionStore<SessionAction>;

impl<TPayload: SessionPayload + Clone + 'static> Default for ActionStore<SessionAction<TPayload>> {
    fn default() -> Self { Self::new() }
}

impl<TPayload: SessionPayload + Clone + 'static> ActionStore<SessionAction<TPayload>> {
    pub fn new() -> Self {
        Self::with_accessors(
            Arc::new(|action: &SessionAction<TPayload>| action.id.clone()),
            Arc::new(|action: &SessionAction<TPayload>| action.delivery),
            Arc::new(|action: &SessionAction<TPayload>| action.lifecycle.state()),
            Arc::new(|action: &SessionAction<TPayload>| action.effective_priority()),
            Arc::new(|action: &mut SessionAction<TPayload>, delivery| action.delivery = delivery),
            Arc::new(|action: &mut SessionAction<TPayload>, next| {
                transition_session_action(action, next, &TransitionOptions::default())
            }),
        )
    }

    pub fn rollback(
        &mut self,
        action: &mut SessionAction<TPayload>,
        proof: Option<RollbackProof>,
    ) -> Result<(), String> {
        transition_session_action(action, ActionLifecycle::Queued, &TransitionOptions { rollback_proof: proof })?;
        self.update_action(action)
    }

    pub fn queue_preview(&self, policy: DeliveryPolicy) -> Vec<String> {
        self.queue_preview_with(policy, &|action| action.payload.preview().to_string())
    }

    pub fn actions_for_message(&self, message: &DeliveryMessage) -> Vec<SessionAction<TPayload>> {
        self.actions(None).into_iter()
            .filter(|action| action.payload.records().iter().any(|record| messages_equal(&record.message, message)))
            .collect()
    }

    // Rust action snapshots are values; write back mutations that TypeScript
    // observes through its shared action object before the next scheduling step.
    pub(crate) fn update_action(&mut self, action: &SessionAction<TPayload>) -> Result<(), String> {
        let stored = self.next_turn_boundary.iter_mut().chain(self.when_run_idle.iter_mut())
            .find(|candidate| candidate.id == action.id)
            .ok_or_else(|| format!("Session action {} is not owned by this store", action.id))?;
        *stored = action.clone();
        Ok(())
    }
}

fn messages_equal(left: &DeliveryMessage, right: &DeliveryMessage) -> bool {
    match (left, right) {
        (DeliveryMessage::User(left), DeliveryMessage::User(right)) => left == right,
        (DeliveryMessage::Custom(left), DeliveryMessage::Custom(right)) => left == right,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeActivity {
    pub lower_agent_run: bool,
    pub compaction: bool,
    pub retry: bool,
    pub bash: bool,
    pub refinement_apply: bool,
    pub branch_mutation: bool,
    pub scheduler_pause_count: i64,
    pub disposing: bool,
}

/// `IdleEvictionMinutes = number | "off"`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IdleEvictionMinutes {
    Minutes(f64),
    Off,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionEvictionSnapshot {
    pub is_session_active: bool,
    pub attached_clients: i64,
    pub has_registered_cron_job: bool,
    pub last_activity_at: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionPassivationSnapshot {
    pub eviction: SessionEvictionSnapshot,
    pub has_parent: bool,
    pub has_non_passive_descendants: bool,
    pub is_hydrating: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerLifecycle {
    Starting,
    Ready,
    Recovering,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkerEvictionSnapshot {
    pub lifecycle: WorkerLifecycle,
    pub is_connected: bool,
    pub is_stopping: bool,
    pub has_owner_client: bool,
    pub is_preparing_update_restart: bool,
    pub has_wake_blind_schedule: bool,
    pub sessions: Vec<SessionEvictionSnapshot>,
}

fn is_idle_eviction_threshold_met(
    session: &SessionEvictionSnapshot,
    idle_eviction_minutes: IdleEvictionMinutes,
    now: f64,
) -> bool {
    let minutes = match idle_eviction_minutes {
        IdleEvictionMinutes::Off => return false,
        IdleEvictionMinutes::Minutes(minutes) => minutes,
    };
    if !minutes.is_finite() || minutes <= 0.0 {
        return false;
    }
    !session.is_session_active
        && session.attached_clients == 0
        && !session.has_registered_cron_job
        && session.last_activity_at.is_finite()
        && now - session.last_activity_at >= minutes * 60_000.0
}

/// Pure per-node residency policy. Roots remain owned by whole-worker eviction.
pub fn can_passivate_session(
    session: &SessionPassivationSnapshot,
    idle_eviction_minutes: IdleEvictionMinutes,
    now: f64,
) -> bool {
    session.has_parent
        && !session.has_non_passive_descendants
        && !session.is_hydrating
        && is_idle_eviction_threshold_met(&session.eviction, idle_eviction_minutes, now)
}

/// Pure whole-tree residency policy. Callers must supply supervisor-owned attachment state.
pub fn can_evict_worker(
    worker: &WorkerEvictionSnapshot,
    idle_eviction_minutes: IdleEvictionMinutes,
    now: f64,
) -> bool {
    if worker.lifecycle != WorkerLifecycle::Ready
        || !worker.is_connected
        || worker.is_stopping
        || worker.has_owner_client
        || worker.is_preparing_update_restart
        || worker.has_wake_blind_schedule
        || worker.sessions.is_empty()
    {
        return false;
    }
    worker
        .sessions
        .iter()
        .all(|session| is_idle_eviction_threshold_met(session, idle_eviction_minutes, now))
}

pub fn can_select_session_action(activity: &RuntimeActivity) -> bool {
    !activity.lower_agent_run
        && !activity.compaction
        && !activity.retry
        && !activity.bash
        && !activity.refinement_apply
        && !activity.branch_mutation
        && activity.scheduler_pause_count == 0
        && !activity.disposing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_message(text: &str) -> DeliveryMessage {
        DeliveryMessage::User(UserMessage::new(
            pi_ai::types::UserContent::Text(text.to_string()),
            1,
        ))
    }

    fn turn_action(id: &str, delivery: DeliveryPolicy, text: &str) -> SessionAction {
        SessionAction {
            id: id.to_string(),
            source: ActionSource::Internal,
            priority: None,
            delivery,
            wake: WakePolicy::Immediate,
            payload: SessionActionPayload::Turn(SessionTurnPayload {
                records: vec![DeliveryRecord {
                    id: format!("{id}-record"),
                    role: DeliveryRecordRole::Primary,
                    message: user_message(text),
                    started: false,
                    durable: false,
                    owner_action_id: id.to_string(),
                }],
                text: text.to_string(),
                preview: None,
            }),
            lifecycle: ActionLifecycle::Queued,
            queue_key: None,
            agent_message_id: None,
            suppress_autonomous_continuation: None,
        }
    }

    fn command_action(id: &str, delivery: DeliveryPolicy, text: &str) -> SessionAction {
        SessionAction {
            id: id.to_string(),
            source: ActionSource::Input(InputSource::Rpc),
            priority: None,
            delivery,
            wake: WakePolicy::OnLowerBoundary,
            payload: SessionActionPayload::SessionCommand(SessionCommandPayload {
                command: SessionSlashCommand {
                    name: "compact".to_string(),
                    args: String::new(),
                    text: text.to_string(),
                },
                text: text.to_string(),
            }),
            lifecycle: ActionLifecycle::Queued,
            queue_key: None,
            agent_message_id: None,
            suppress_autonomous_continuation: None,
        }
    }

    #[test]
    fn lane_delivery_policy_matches_the_typescript() {
        assert_eq!(
            queued_message_lane_delivery_policy(QueuedMessageLane::Steering),
            DeliveryPolicy::NextTurnBoundary
        );
        assert_eq!(
            queued_message_lane_delivery_policy(QueuedMessageLane::FollowUp),
            DeliveryPolicy::WhenRunIdle
        );
    }

    #[test]
    fn backlog_human_priority_preserves_fifo_active_work_and_recovery_order() {
        let lane = DeliveryPolicy::NextTurnBoundary;
        let mut store = SessionActionStore::new();
        store.enqueue(turn_action("active", lane, "already selected")).unwrap();
        store.select_first().unwrap();
        store.enqueue(turn_action("background", lane, "machine")).unwrap();
        let mut pinned = turn_action("pinned", lane, "goal");
        pinned.priority = Some(SessionActionPriority::Pinned);
        store.enqueue_front(pinned).unwrap();
        for id in ["human-1", "human-2"] {
            let mut human = turn_action(id, lane, id);
            human.source = ActionSource::Input(InputSource::Interactive);
            human.agent_message_id = Some(format!("prompt-wait-{id}"));
            store.enqueue(human).unwrap();
        }
        let mut machine = turn_action("agent", lane, "agent traffic");
        machine.source = ActionSource::Input(InputSource::Rpc);
        machine.agent_message_id = Some("agentmsg_child".into());
        store.enqueue(machine).unwrap();
        assert_eq!(store.actions(None).iter().map(|action| action.id.as_str()).collect::<Vec<_>>(),
                   vec!["active", "pinned", "human-1", "human-2", "background", "agent"]);
        let mut recovered = SessionActionStore::new();
        for action in store.queued_actions(None) { recovered.enqueue_tail(action).unwrap(); }
        assert_eq!(recovered.queued_actions(None), store.queued_actions(None));
        let mut legacy = serde_json::to_value(turn_action("old", lane, "old")).unwrap();
        legacy.as_object_mut().unwrap().remove("priority");
        let legacy: SessionAction = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.effective_priority(), SessionActionPriority::Background);

        // A saved/manual order need not already be sorted by priority. A new
        // human message still may not leap over an earlier human message.
        let mut restored = SessionActionStore::new();
        restored.enqueue_tail(turn_action("old-background", lane, "old")).unwrap();
        let mut old_human = turn_action("old-human", lane, "old");
        old_human.source = ActionSource::Input(InputSource::Interactive);
        restored.enqueue_tail(old_human).unwrap();
        restored.enqueue_tail(turn_action("tail-background", lane, "tail")).unwrap();
        let mut new_human = turn_action("new-human", lane, "new");
        new_human.source = ActionSource::Input(InputSource::Interactive);
        restored.enqueue(new_human).unwrap();
        assert_eq!(restored.actions(None).iter().map(|action| action.id.as_str()).collect::<Vec<_>>(),
                   vec!["old-background", "old-human", "new-human", "tail-background"]);
    }

    #[test]
    fn legal_transitions_are_enforced() {
        let mut action = turn_action("a", DeliveryPolicy::NextTurnBoundary, "one");
        assert!(transition_session_action(
            &mut action,
            ActionLifecycle::Selected,
            &TransitionOptions::default()
        )
        .is_ok());
        assert_eq!(action.lifecycle.state(), ActionLifecycleState::Selected);
        let error = transition_session_action(
            &mut action,
            ActionLifecycle::Completed,
            &TransitionOptions::default(),
        )
        .unwrap_err();
        assert_eq!(
            error,
            "Illegal session action lifecycle transition: selected -> completed"
        );
    }

    #[test]
    fn committing_rollback_requires_settled_dispatch_and_clean_transcript() {
        let mut action = turn_action("a", DeliveryPolicy::NextTurnBoundary, "one");
        action.lifecycle = ActionLifecycle::Committing;

        let error = transition_session_action(
            &mut action,
            ActionLifecycle::Queued,
            &TransitionOptions::default(),
        )
        .unwrap_err();
        assert_eq!(
            error,
            "Committing session action rollback requires a settled dispatch and transcript proof"
        );

        let proof = RollbackProof {
            dispatch_settled: true,
            transcript: Vec::new(),
        };
        assert!(transition_session_action(
            &mut action,
            ActionLifecycle::Queued,
            &TransitionOptions {
                rollback_proof: Some(proof)
            }
        )
        .is_ok());
    }

    #[test]
    fn enqueue_rejects_duplicates_and_nonqueued_actions() {
        let mut store = SessionActionStore::new();
        store
            .enqueue(turn_action("a", DeliveryPolicy::NextTurnBoundary, "one"))
            .unwrap();
        let error = store
            .enqueue(turn_action("a", DeliveryPolicy::NextTurnBoundary, "one"))
            .unwrap_err();
        assert_eq!(error, "Duplicate session action id: a");
        let mut selected = turn_action("b", DeliveryPolicy::NextTurnBoundary, "two");
        selected.lifecycle = ActionLifecycle::Selected;
        assert_eq!(
            store.enqueue(selected).unwrap_err(),
            "Only queued session actions can be enqueued"
        );
    }

    #[test]
    fn select_first_prefers_the_next_turn_boundary_lane() {
        let mut store = SessionActionStore::new();
        store
            .enqueue(turn_action("follow", DeliveryPolicy::WhenRunIdle, "f"))
            .unwrap();
        store
            .enqueue(turn_action("steer", DeliveryPolicy::NextTurnBoundary, "s"))
            .unwrap();
        let selected = store.select_first().unwrap().unwrap();
        assert_eq!(selected.id, "steer");
        assert_eq!(selected.lifecycle.state(), ActionLifecycleState::Selected);
        let next = store.select_first().unwrap().unwrap();
        assert_eq!(next.id, "follow");
        assert!(store.select_first().unwrap().is_none());
    }

    #[test]
    fn enqueue_front_lands_before_the_first_queued_action() {
        let mut store = SessionActionStore::new();
        store
            .enqueue(turn_action("first", DeliveryPolicy::NextTurnBoundary, "1"))
            .unwrap();
        store
            .enqueue(turn_action("second", DeliveryPolicy::NextTurnBoundary, "2"))
            .unwrap();
        store
            .enqueue_front(turn_action("front", DeliveryPolicy::NextTurnBoundary, "0"))
            .unwrap();
        let ids: Vec<String> = store
            .owned_actions()
            .into_iter()
            .map(|action| action.id)
            .collect();
        assert_eq!(ids, vec!["front", "first", "second"]);
    }

    #[test]
    fn remove_cancels_matching_actions() {
        let mut store = SessionActionStore::new();
        store
            .enqueue(turn_action("a", DeliveryPolicy::NextTurnBoundary, "one"))
            .unwrap();
        store
            .enqueue(turn_action("b", DeliveryPolicy::NextTurnBoundary, "two"))
            .unwrap();
        let removed = store
            .remove(&|action: &SessionAction| action.id == "a", None)
            .unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(
            removed[0].lifecycle.state(),
            ActionLifecycleState::Cancelled
        );
        assert_eq!(store.owned_actions().len(), 2);
        assert_eq!(store.queued_actions(None).len(), 1);
        store.release_terminal(&removed[0]).unwrap();
        assert_eq!(store.owned_actions().len(), 1);
    }

    #[test]
    fn swap_and_move_only_apply_to_queued_actions_in_one_lane() {
        let mut store = SessionActionStore::new();
        let a = turn_action("a", DeliveryPolicy::NextTurnBoundary, "one");
        let b = turn_action("b", DeliveryPolicy::NextTurnBoundary, "two");
        let c = turn_action("c", DeliveryPolicy::WhenRunIdle, "three");
        store.enqueue(a.clone()).unwrap();
        store.enqueue(b.clone()).unwrap();
        store.enqueue(c.clone()).unwrap();
        store.swap_queued(&a, &b).unwrap();
        let ids: Vec<String> = store
            .owned_actions()
            .into_iter()
            .map(|action| action.id)
            .collect();
        assert_eq!(ids, vec!["b", "a", "c"]);
        assert_eq!(
            store.swap_queued(&a, &c).unwrap_err(),
            "Only queued actions in the same lane can be swapped"
        );

        let mut moved = a.clone();
        store
            .move_queued(&mut moved, DeliveryPolicy::WhenRunIdle, 0)
            .unwrap();
        let queued_idle = store.queued_actions(Some(DeliveryPolicy::WhenRunIdle));
        assert_eq!(
            queued_idle
                .iter()
                .map(|action| action.id.clone())
                .collect::<Vec<_>>(),
            vec!["a", "c"]
        );

        let mut selected = b.clone();
        selected.lifecycle = ActionLifecycle::Selected;
        assert_eq!(
            store
                .move_queued(&mut selected, DeliveryPolicy::WhenRunIdle, 0)
                .unwrap_err(),
            "Only queued actions can be moved"
        );
    }

    #[test]
    fn queue_preview_uses_preview_then_text() {
        let mut store = SessionActionStore::new();
        store
            .enqueue(turn_action("a", DeliveryPolicy::NextTurnBoundary, "one"))
            .unwrap();
        store
            .enqueue(command_action(
                "cmd",
                DeliveryPolicy::NextTurnBoundary,
                "/compact",
            ))
            .unwrap();
        assert_eq!(
            store.queue_preview(DeliveryPolicy::NextTurnBoundary),
            vec!["/compact".to_string(), "one".to_string()]
        );
    }

    #[test]
    fn tickets_settle_once_and_report_the_first_outcome() {
        let controller = ActionTicketController::new("a");
        assert!(controller.settle_accepted(SubmissionOutcome::HandledWithoutTurn));
        assert!(!controller.settle_accepted(SubmissionOutcome::HandledWithoutTurn));
        assert!(controller.settle_delivered(DeliveryOutcome::Delivered));
        assert!(!controller.settle_delivered(DeliveryOutcome::NotApplicable));
        assert!(controller.settle_completed(None));
        assert!(!controller.settle_completed(None));
        assert!(!controller.settle_completed(Some("late error".to_string())));
        assert_eq!(controller.completed_error(), None);
    }

    #[tokio::test]
    async fn ticket_promises_retain_rejections_for_every_awaiter() {
        let controller = Arc::new(ActionTicketController::new("a"));
        let first = controller.ticket.clone();
        let second = controller.ticket.clone();
        assert!(controller.reject_delivered("delivery failed".to_string()));
        assert!(controller.settle_completed(Some("completion failed".to_string())));
        assert!(!controller.settle_completed(Some("late error".to_string())));
        assert_eq!(first.delivered.await, Err("delivery failed".to_string()));
        assert_eq!(second.delivered.await, Err("delivery failed".to_string()));
        assert_eq!(first.completed.await, Err("completion failed".to_string()));
        assert_eq!(second.completed.await, Err("completion failed".to_string()));
        assert_eq!(controller.completed_error().as_deref(), Some("completion failed"));
    }

    #[test]
    fn generic_actions_keep_preparation_and_rollback_the_stored_action() {
        #[derive(Clone)]
        struct Prepared {
            payload: SessionActionPayload,
            preparation: String,
        }
        impl SessionPayload for Prepared {
            fn records(&self) -> &[DeliveryRecord] { self.payload.records() }
            fn preview(&self) -> &str { self.payload.preview() }
        }
        let original = turn_action("prepared", DeliveryPolicy::WhenRunIdle, "prompt");
        let mut store = ActionStore::new();
        store.enqueue(SessionAction {
            id: original.id,
            source: original.source,
            priority: original.priority,
            delivery: original.delivery,
            wake: original.wake,
            payload: Prepared { payload: original.payload, preparation: "prepared content".to_string() },
            lifecycle: original.lifecycle,
            queue_key: original.queue_key,
            agent_message_id: original.agent_message_id,
            suppress_autonomous_continuation: original.suppress_autonomous_continuation,
        }).unwrap();
        let mut selected = store.select_first().unwrap().unwrap();
        assert_eq!(selected.payload.preparation, "prepared content");
        store.rollback(&mut selected, None).unwrap();
        assert_eq!(store.queued_actions(None).len(), 1);
        assert_eq!(store.queue_preview(DeliveryPolicy::WhenRunIdle), vec!["prompt"]);
        assert_eq!(store.select_first().unwrap().unwrap().payload.preparation, "prepared content");
    }

    #[test]
    fn explicit_remove_candidates_keep_order_and_include_running_actions() {
        let mut store = SessionActionStore::new();
        store.enqueue(turn_action("a", DeliveryPolicy::WhenRunIdle, "first")).unwrap();
        store.enqueue(turn_action("b", DeliveryPolicy::WhenRunIdle, "second")).unwrap();
        let mut first = store.select_first().unwrap().unwrap();
        transition_session_action(&mut first, ActionLifecycle::Running { execution: ActionExecution::AgentTurn }, &TransitionOptions::default()).unwrap();
        store.update_action(&first).unwrap();
        let second = store.queued_actions(None).pop().unwrap();
        let removed = store.remove(&|_| true, Some(&[second, first])).unwrap();
        assert_eq!(removed.iter().map(|action| action.id.as_str()).collect::<Vec<_>>(), vec!["b", "a"]);
        assert!(store.unfinished_actions(None).is_empty());
        assert_eq!(store.owned_actions().len(), 2);
    }

    #[test]
    fn action_serialization_uses_shared_custom_message_and_typescript_fields() {
        let message = CustomMessage {
            role: "custom".to_string(),
            custom_type: "notice".to_string(),
            content: pi_agent_core::types::CustomMessageContent::Text("notice".to_string()),
            display: true,
            details: None,
            timestamp: 1,
        };
        let mut action = turn_action("a", DeliveryPolicy::WhenRunIdle, "notice");
        if let SessionActionPayload::Turn(turn) = &mut action.payload {
            turn.records[0].message = DeliveryMessage::Custom(message.clone());
        }
        let value = serde_json::to_value(&action).unwrap();
        assert_eq!(value["payload"]["kind"], "turn");
        assert_eq!(value["payload"]["records"][0]["ownerActionId"], "a");
        assert!(value.get("queueKey").is_none());
        assert_eq!(serde_json::from_value::<SessionAction>(value).unwrap(), action);
        action.lifecycle = ActionLifecycle::Committing;
        let error = transition_session_action(&mut action, ActionLifecycle::Queued, &TransitionOptions {
            rollback_proof: Some(RollbackProof {
                dispatch_settled: true,
                transcript: vec![crate::core::messages::custom_message_to_agent_message(message)],
            }),
        }).unwrap_err();
        assert!(error.contains("primary message is durable"));
    }

    #[test]
    fn ticket_for_rejects_unknown_actions_and_release_requires_terminal() {
        let mut store = SessionActionStore::new();
        let action = turn_action("a", DeliveryPolicy::NextTurnBoundary, "one");
        store.enqueue(action.clone()).unwrap();
        assert!(store.ticket_for(&action).is_ok());
        let unknown = turn_action("z", DeliveryPolicy::NextTurnBoundary, "z");
        assert_eq!(
            store.ticket_for(&unknown).err().unwrap(),
            "Session action z is not owned by this store"
        );
        assert_eq!(
            store.release_terminal(&action).unwrap_err(),
            "Cannot release nonterminal session action a"
        );
        let mut cancelled = action.clone();
        cancelled.lifecycle = ActionLifecycle::Cancelled;
        store.release_terminal(&cancelled).unwrap();
        assert!(store.owned_actions().is_empty());
    }

    #[test]
    fn actions_for_message_matches_owned_records() {
        let mut store = SessionActionStore::new();
        let action = turn_action("a", DeliveryPolicy::NextTurnBoundary, "one");
        store.enqueue(action.clone()).unwrap();
        let matched = store.actions_for_message(&user_message("one"));
        assert_eq!(matched.len(), 1);
        assert!(store.actions_for_message(&user_message("other")).is_empty());
    }

    #[test]
    fn idle_eviction_thresholds_and_policies() {
        let session = SessionEvictionSnapshot {
            is_session_active: false,
            attached_clients: 0,
            has_registered_cron_job: false,
            last_activity_at: 0.0,
        };
        assert!(!is_idle_eviction_threshold_met(
            &session,
            IdleEvictionMinutes::Off,
            60_000.0
        ));
        assert!(!is_idle_eviction_threshold_met(
            &session,
            IdleEvictionMinutes::Minutes(0.0),
            60_000.0
        ));
        assert!(!is_idle_eviction_threshold_met(
            &session,
            IdleEvictionMinutes::Minutes(1.0),
            59_999.0
        ));
        assert!(is_idle_eviction_threshold_met(
            &session,
            IdleEvictionMinutes::Minutes(1.0),
            60_000.0
        ));

        let passivation = SessionPassivationSnapshot {
            eviction: session.clone(),
            has_parent: true,
            has_non_passive_descendants: false,
            is_hydrating: false,
        };
        assert!(can_passivate_session(
            &passivation,
            IdleEvictionMinutes::Minutes(1.0),
            60_000.0
        ));
        let mut rooted = passivation.clone();
        rooted.has_parent = false;
        assert!(!can_passivate_session(
            &rooted,
            IdleEvictionMinutes::Minutes(1.0),
            60_000.0
        ));

        let worker = WorkerEvictionSnapshot {
            lifecycle: WorkerLifecycle::Ready,
            is_connected: true,
            is_stopping: false,
            has_owner_client: false,
            is_preparing_update_restart: false,
            has_wake_blind_schedule: false,
            sessions: vec![session.clone()],
        };
        assert!(can_evict_worker(
            &worker,
            IdleEvictionMinutes::Minutes(1.0),
            60_000.0
        ));
        let mut busy = worker.clone();
        busy.has_owner_client = true;
        assert!(!can_evict_worker(
            &busy,
            IdleEvictionMinutes::Minutes(1.0),
            60_000.0
        ));
        let mut empty = worker.clone();
        empty.sessions.clear();
        assert!(!can_evict_worker(
            &empty,
            IdleEvictionMinutes::Minutes(1.0),
            60_000.0
        ));
        let mut starting = worker.clone();
        starting.lifecycle = WorkerLifecycle::Starting;
        assert!(!can_evict_worker(
            &starting,
            IdleEvictionMinutes::Minutes(1.0),
            60_000.0
        ));
    }

    #[test]
    fn action_selection_requires_a_fully_idle_runtime() {
        let idle = RuntimeActivity {
            lower_agent_run: false,
            compaction: false,
            retry: false,
            bash: false,
            refinement_apply: false,
            branch_mutation: false,
            scheduler_pause_count: 0,
            disposing: false,
        };
        assert!(can_select_session_action(&idle));
        for mutate in [
            |activity: &mut RuntimeActivity| activity.lower_agent_run = true,
            |activity: &mut RuntimeActivity| activity.compaction = true,
            |activity: &mut RuntimeActivity| activity.retry = true,
            |activity: &mut RuntimeActivity| activity.bash = true,
            |activity: &mut RuntimeActivity| activity.refinement_apply = true,
            |activity: &mut RuntimeActivity| activity.branch_mutation = true,
            |activity: &mut RuntimeActivity| activity.scheduler_pause_count = 1,
            |activity: &mut RuntimeActivity| activity.disposing = true,
        ] {
            let mut busy = idle.clone();
            mutate(&mut busy);
            assert!(!can_select_session_action(&busy));
        }
    }
}
