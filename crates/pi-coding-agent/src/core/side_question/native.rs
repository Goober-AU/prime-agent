//! Adapt the existing side-question/retry lifecycle to the real async Agent.
use super::*;
use crate::core::agent_session::AgentHandle;
use pi_agent_core::agent::{Agent, AgentOptions, PromptInput};

struct Parent(Arc<dyn AgentHandle>);
impl SideQuestionParent for Parent {
    fn state(&self) -> AgentState {
        self.0.state()
    }
    // The native factory below copies the async callbacks directly; the
    // synchronous compatibility callbacks cannot carry their futures.
    fn convert_to_llm(
        &self,
    ) -> Option<Arc<dyn Fn(Vec<AgentMessage>) -> Vec<pi_ai::types::Message> + Send + Sync>> {
        None
    }
    fn transform_context(
        &self,
    ) -> Option<
        Arc<
            dyn Fn(Vec<AgentMessage>, Option<CancellationToken>) -> Vec<AgentMessage> + Send + Sync,
        >,
    > {
        None
    }
    fn stream_fn(&self) -> StreamFn {
        self.0.stream_fn()
    }
    fn get_api_key(&self) -> Option<Arc<dyn Fn(String) -> Option<String> + Send + Sync>> {
        None
    }
    fn on_payload(&self) -> Option<pi_ai::types::OnPayload> {
        None
    }
    fn on_response(&self) -> Option<pi_ai::types::OnResponse> {
        None
    }
    fn tool_execution(&self) -> Option<String> {
        None
    }
    fn session_id(&self) -> Option<String> {
        None
    }
    fn thinking_budgets(&self) -> Option<pi_ai::types::ThinkingBudgets> {
        None
    }
}
struct SideAgent(Arc<Agent>);
impl SideQuestionAgent for SideAgent {
    fn state(&self) -> AgentState {
        self.0.state()
    }
    fn set_state(&self, state: AgentState) {
        self.0.set_state(state);
    }
    fn subscribe(
        &self,
        listener: Arc<dyn Fn(AgentEvent, Option<CancellationToken>) -> BoxFuture<()> + Send + Sync>,
    ) -> Arc<dyn Fn() + Send + Sync> {
        let subscription = self.0.subscribe(listener);
        Arc::new(move || subscription.unsubscribe())
    }
    fn prompt(&self, messages: Vec<AgentMessage>) -> BoxFuture<anyhow::Result<()>> {
        let agent = self.0.clone();
        Box::pin(async move { agent.prompt(PromptInput::Messages(messages)).await })
    }
    fn continue_(&self) -> BoxFuture<Result<(), String>> {
        let agent = self.0.clone();
        Box::pin(async move { agent.continue_().await.map_err(|e| e.to_string()) })
    }
    fn abort(&self) {
        self.0.abort();
    }
}
pub(crate) fn start(
    parent: Arc<dyn AgentHandle>,
    id: String,
    question: String,
    on_event: Arc<dyn Fn(SideQuestionEvent) -> BoxFuture<()> + Send + Sync>,
    previous: Option<Vec<SideQuestionTurn>>,
    retry: Option<ProviderRetryPolicy>,
) -> Result<SideQuestionRun, String> {
    let native = parent
        .side_question_options()
        .ok_or("This agent does not expose the callbacks required for side questions")?;
    let factory: SideQuestionAgentFactory = Arc::new(move |options| {
        Arc::new(SideAgent(Agent::new(AgentOptions {
            initial_state: Some(options.initial_state),
            convert_to_llm: native.convert_to_llm.clone(),
            transform_context: native.transform_context.clone(),
            stream_fn: options.stream_fn,
            get_api_key: native.get_api_key.clone(),
            on_payload: native.on_payload.clone(),
            on_response: native.on_response.clone(),
            should_stop_after_turn: Some(Arc::new(|_| Box::pin(async { true }))),
            session_id: native.session_id.clone(),
            thinking_budgets: native.thinking_budgets.clone(),
            transport: Some("sse".into()),
            tool_execution: native.tool_execution,
            ..Default::default()
        })))
    });
    start_side_question(
        Arc::new(Parent(parent)),
        factory,
        id,
        question,
        on_event,
        previous,
        retry,
    )
}
