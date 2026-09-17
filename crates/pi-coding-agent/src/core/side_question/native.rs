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
        let stop = options.should_stop_after_turn;
        Arc::new(SideAgent(Agent::new(AgentOptions {
            initial_state: Some(options.initial_state),
            convert_to_llm: native.convert_to_llm.clone(),
            transform_context: native.transform_context.clone(),
            stream_fn: options.stream_fn,
            get_api_key: native.get_api_key.clone(),
            on_payload: native.on_payload.clone(),
            on_response: native.on_response.clone(),
            before_tool_call: Some(Arc::new(|_, _| Box::pin(async {
                Ok(Some(pi_agent_core::types::BeforeToolCallResult {
                    block: Some(true),
                    reason: Some(SIDE_QUESTION_TOOL_BLOCKED.to_string()),
                }))
            }))),
            should_stop_after_turn: Some(Arc::new(move |context| {
                let stopped = stop(context);
                Box::pin(async move { stopped })
            })),
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

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::{AssistantMessageEvent, ContentBlock, TextContent, ToolCall};

    #[tokio::test]
    async fn backlog_side_question_retains_cache_tools_but_blocks_execution_and_keeps_reasoning() {
        let executed = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let mut model = pi_ai::types::Model::new("test", "test", "test", "test", "");
        model.reasoning = true;
        let tool = pi_agent_core::types::AgentTool {
            name: "ipython".into(), description: "python".into(), label: "python".into(),
            parameters: serde_json::json!({"type":"object"}), prepare_arguments: None,
            execution_mode: None,
            execute: Arc::new({ let executed = executed.clone(); move |_, _, _, _| {
                executed.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(anyhow::anyhow!("must never execute")) })
            }}),
        };
        let parent = Agent::new(AgentOptions {
            initial_state: Some(AgentState { model, thinking_level: pi_agent_core::types::ThinkingLevel::Max,
                tools: Some(vec![tool]), ..Default::default() }),
            get_api_key: Some(Arc::new(|_| Box::pin(async { Some("fake".into()) }))),
            stream_fn: Some(Arc::new({ let requests = requests.clone(); move |model, context, options| {
                let turn = requests.fetch_add(1, Ordering::SeqCst);
                assert_eq!(options.reasoning.as_deref(), Some("low"));
                assert_eq!(context.tools.as_ref().unwrap()[0].name, "ipython");
                if turn > 0 {
                    assert!(context.messages.iter().any(|message| matches!(message, pi_ai::types::Message::ToolResult(result) if result.is_error)));
                }
                Box::pin(async move {
                    let output = AssistantMessage { api: model.api, provider: model.provider, model: model.id,
                        content: if turn == 0 { vec![ContentBlock::ToolCall(ToolCall::new("call", "ipython", Default::default()))] }
                                 else { vec![ContentBlock::Text(TextContent::new("Side answer"))] },
                        stop_reason: if turn == 0 { "toolUse" } else { "stop" }.into(), ..Default::default() };
                    let stream = pi_ai::utils::event_stream::create_assistant_message_event_stream();
                    stream.push(AssistantMessageEvent::Done { reason: output.stop_reason.clone(), message: output });
                    stream.end(None);
                    stream
                })
            }})),
            ..Default::default()
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let run = start(Arc::new(parent.clone()), "test".into(), "why?".into(), Arc::new({ let events = events.clone(); move |event| {
            events.lock().unwrap().push(event); Box::pin(async {})
        }}), None, None).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), run.done).await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert_eq!(executed.load(Ordering::SeqCst), 0);
        assert!(parent.state().messages.is_empty());
        let final_event = events.lock().unwrap().last().cloned().unwrap();
        assert_eq!(final_event.status, SIDE_QUESTION_STATUS_COMPLETE);
        assert_eq!(final_event.answer, "Side answer");
    }
}
