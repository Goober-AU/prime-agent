//! UI state owned by the terminal thread. Daemon notifications carry data;
//! components and their lifetime stay here, as in the TypeScript interactive host.
use super::*;
use crate::modes::interactive::components::side_question::SideQuestionComponent;

#[derive(Default)]
pub(super) struct Surfaces {
    statuses: indexmap::IndexMap<String, String>,
    widgets: indexmap::IndexMap<String, (bool, Box<dyn TuiComponent>)>,
    pub header: Option<Box<dyn TuiComponent>>,
    pub footer: Option<Box<dyn TuiComponent>>,
}
struct Lines(Vec<String>);
impl TuiComponent for Lines {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.0
            .iter()
            .flat_map(|line| TuiText::new(line.clone(), 1, 0, None).render(width))
            .collect()
    }
    fn invalidate(&mut self) {}
}
impl Surfaces {
    pub fn set_status(&mut self, key: String, text: Option<String>) {
        if let Some(text) = text {
            self.statuses.insert(key, text);
        } else {
            self.statuses.shift_remove(&key);
        }
    }
    pub fn set_widget(&mut self, key: String, lines: Option<Vec<String>>, below: bool) {
        self.widgets.shift_remove(&key);
        if let Some(mut lines) = lines {
            if lines.len() > 10 {
                lines.truncate(10);
                lines.push(theme().fg("muted", "... (widget truncated)"));
            }
            self.widgets.insert(key, (below, Box::new(Lines(lines))));
        }
    }
    pub fn set_widget_component(
        &mut self,
        key: String,
        component: Option<Box<dyn TuiComponent>>,
        below: bool,
    ) {
        self.widgets.shift_remove(&key);
        if let Some(component) = component {
            self.widgets.insert(key, (below, component));
        }
    }
    pub fn reset(&mut self) {
        self.widgets.clear();
        self.statuses.clear();
        self.footer = None;
        self.header = None;
    }
}
pub(super) struct Widgets(pub Rc<RefCell<Surfaces>>, pub bool);
impl TuiComponent for Widgets {
    fn render(&mut self, width: f64) -> Vec<String> {
        let mut lines = Vec::new();
        for (below, content) in self.0.borrow_mut().widgets.values_mut() {
            if *below == self.1 {
                lines.extend(content.render(width));
            }
        }
        lines
    }
    fn invalidate(&mut self) {}
}
pub(super) struct Statuses(pub Rc<RefCell<Surfaces>>, pub(super) Tray);
impl TuiComponent for Statuses {
    fn render(&mut self, width: f64) -> Vec<String> {
        let mut state = self.0.borrow_mut();
        if let Some(footer) = &mut state.footer {
            return footer.render(width);
        }
        let mut lines = self.1.render(width);
        if !state.statuses.is_empty() {
            lines.extend(
                TuiText::new(
                    state
                        .statuses
                        .values()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" "),
                    1,
                    0,
                    None,
                )
                .render(width),
            );
        }
        lines
    }
    fn invalidate(&mut self) {}
}

#[derive(Default)]
pub(super) struct SidePane {
    component: Option<SideQuestionComponent>,
    turns: Vec<wire::AgentConnectionSideQuestionEvent>,
}
impl SidePane {
    pub fn is_open(&self) -> bool {
        self.component.is_some()
    }
    pub fn running(&self) -> bool {
        self.turns.iter().any(|t| t.status == "running")
    }
    pub fn start(
        &mut self,
        question: String,
        connection: Arc<dyn wire::AgentConnection>,
        send: mpsc::Sender<HostEvent>,
    ) {
        if question.trim().is_empty() {
            let _ = send.send(HostEvent::Error("Usage: /btw <question>".into()));
            return;
        }
        if self.running() {
            let _ = send.send(HostEvent::Warning(
                "Wait for the side answer, or press Esc to cancel it.".into(),
            ));
            return;
        }
        let previous = self
            .turns
            .iter()
            .filter(|t| !t.answer.is_empty())
            .map(|t| wire::AgentConnectionSideQuestionTurn {
                question: t.question.clone(),
                answer: t.answer.clone(),
            })
            .collect();
        let event = wire::AgentConnectionSideQuestionEvent {
            id: uuid::Uuid::new_v4().to_string(),
            question,
            answer: String::new(),
            status: "running".into(),
            error_message: None,
        };
        match &mut self.component {
            Some(component) => component.add_turn(event.clone()),
            None => self.component = Some(SideQuestionComponent::new(event.clone(), None)),
        }
        self.turns.push(event.clone());
        tokio::spawn(async move {
            if let Err(error) = connection
                .start_side_question(&event.id, &event.question, Some(previous))
                .await
            {
                let _ = send.send(HostEvent::Connection(
                    wire::AgentConnectionEvent::SideQuestionEvent {
                        event: wire::AgentConnectionSideQuestionEvent {
                            status: "error".into(),
                            error_message: Some(error),
                            ..event
                        },
                    },
                ));
            }
        });
    }
    pub fn update(&mut self, event: wire::AgentConnectionSideQuestionEvent) {
        if let Some(turn) = self.turns.iter_mut().find(|t| t.id == event.id) {
            *turn = event.clone();
            if let Some(component) = &mut self.component {
                component.update(event);
            }
        }
    }
    pub fn close(&mut self, connection: Arc<dyn wire::AgentConnection>) {
        for turn in self.turns.drain(..).filter(|t| t.status == "running") {
            let connection = connection.clone();
            tokio::spawn(async move {
                let _ = connection.abort_side_question(&turn.id).await;
            });
        }
        self.component = None;
    }
}
impl TuiComponent for SidePane {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.component
            .as_mut()
            .map(|c| c.render(width))
            .unwrap_or_default()
    }
    fn invalidate(&mut self) {
        if let Some(component) = &mut self.component {
            component.invalidate();
        }
    }
}
