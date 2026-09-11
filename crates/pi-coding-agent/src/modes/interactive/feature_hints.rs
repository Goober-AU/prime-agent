//! Port of packages/coding-agent/src/modes/interactive/feature-hints.ts

/// `FeatureHintContext`
pub struct FeatureHintContext {
    pub get_keybinding: Box<dyn Fn(&str) -> Option<String> + Send + Sync>,
    pub is_resident_session: bool,
}

impl FeatureHintContext {
    fn keybinding(&self, action: &str) -> Option<String> {
        (self.get_keybinding)(action)
    }
}

/// `FeatureHintDefinition`
pub struct FeatureHintDefinition {
    pub id: &'static str,
    pub get_text: fn(&FeatureHintContext) -> Option<String>,
}

/// `FeatureHint`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureHint {
    pub id: String,
    pub text: String,
}

/// `FEATURE_HINTS`
pub const FEATURE_HINTS: &[FeatureHintDefinition] = &[
    FeatureHintDefinition {
        id: "side-question",
        get_text: |_| Some("Use /btw <question> to ask questions without interrupting your agent.".to_string()),
    },
    FeatureHintDefinition {
        id: "prompt-stash",
        get_text: |context| {
            let key = context.keybinding("app.prompt.stash");
            key.map(|key| format!("Press {key} to stash your prompt and restore it later."))
        },
    },
    FeatureHintDefinition {
        id: "follow-up",
        get_text: |context| {
            let key = context.keybinding("app.message.followUp");
            key.map(|key| format!("Press {key} to send a message after your agent finishes."))
        },
    },
    FeatureHintDefinition {
        id: "heartbeat",
        get_text: |_| Some("Use /heartbeat every 10m <instruction> to repeat a task on a schedule.".to_string()),
    },
    FeatureHintDefinition {
        id: "subagents",
        get_text: |_| Some("Prime Agent can delegate tasks to subagents and run them in parallel.".to_string()),
    },
    FeatureHintDefinition {
        id: "agents-view",
        get_text: |context| {
            if !context.is_resident_session {
                return None;
            }
            let key = context.keybinding("app.agents.back");
            key.map(|key| format!("Hit {key} for Session View: search running, idle, and inactive sessions."))
        },
    },
    FeatureHintDefinition {
        id: "session-rewind",
        get_text: |_| Some("Run /tree to open the session tree and return to a previous message.".to_string()),
    },
    FeatureHintDefinition {
        id: "steering",
        get_text: |_| Some("Send a message while your agent works to steer its current task.".to_string()),
    },
    FeatureHintDefinition {
        id: "agent-messaging",
        get_text: |_| Some("Agents can message each other to share context and coordinate their work.".to_string()),
    },
    FeatureHintDefinition {
        id: "goal",
        get_text: |_| Some("Use /goal <objective> to keep your agent working until the goal is complete.".to_string()),
    },
    FeatureHintDefinition {
        id: "refine",
        get_text: |_| {
            Some("Use /refine to turn useful lessons into reusable skills, memory, and prompts.".to_string())
        },
    },
    FeatureHintDefinition {
        id: "trace-sharing",
        get_text: |_| Some("Share traces with Prime Intellect using /traces on to train open-source LLMs.".to_string()),
    },
    FeatureHintDefinition {
        id: "persistent-ipython",
        get_text: |_| Some("Compaction removes kernel variables over 16 MiB; smaller state persists.".to_string()),
    },
    FeatureHintDefinition {
        id: "context-usage",
        get_text: |_| Some("Use /context to check token usage, cost, and remaining context.".to_string()),
    },
    FeatureHintDefinition {
        id: "session-fork",
        get_text: |_| Some("Use /fork to start a new session from an earlier prompt.".to_string()),
    },
    FeatureHintDefinition {
        id: "compaction",
        get_text: |_| Some("Use /compact <instructions> to summarize old messages and free up context.".to_string()),
    },
    FeatureHintDefinition {
        id: "auto-compaction",
        get_text: |_| Some("Prime Agent automatically compacts long sessions before context fills up.".to_string()),
    },
    FeatureHintDefinition {
        id: "auto-refine",
        get_text: |_| Some("Prime Agent self-improves by refining skills, memories, prompts, and subagents.".to_string()),
    },
    FeatureHintDefinition {
        id: "background-running",
        get_text: |context| {
            if context.is_resident_session {
                Some("You can close the terminal while your agent keeps running in the background.".to_string())
            } else {
                None
            }
        },
    },
];

/// `random()` injected so the shuffle stays testable.
pub type RandomFn = Box<dyn Fn() -> f64 + Send + Sync>;

/// Port of `FeatureHintDeck`.
pub struct FeatureHintDeck {
    remaining: Vec<FeatureHint>,
    previous_id: Option<String>,
    random: RandomFn,
}

impl Default for FeatureHintDeck {
    fn default() -> Self {
        Self::new(Box::new(|| rand::random::<f64>()))
    }
}

impl FeatureHintDeck {
    pub fn new(random: RandomFn) -> Self {
        Self { remaining: Vec::new(), previous_id: None, random }
    }

    /// Port of `next`.
    pub fn next(&mut self, context: &FeatureHintContext) -> Option<FeatureHint> {
        if self.remaining.is_empty() {
            self.refill(context);
        }
        let hint = self.remaining.pop();
        if let Some(hint) = &hint {
            self.previous_id = Some(hint.id.clone());
        }
        hint
    }

    /// Port of `refill`.
    fn refill(&mut self, context: &FeatureHintContext) {
        let mut hints: Vec<FeatureHint> = FEATURE_HINTS
            .iter()
            .filter_map(|hint| (hint.get_text)(context).map(|text| FeatureHint { id: hint.id.to_string(), text }))
            .collect();
        let mut index = hints.len();
        while index > 1 {
            index -= 1;
            let target = ((self.random)() * (index + 1) as f64).floor() as usize;
            let target = target.min(index);
            hints.swap(index, target);
        }
        if hints.len() > 1 && hints.last().map(|hint| hint.id.as_str()) == self.previous_id.as_deref() {
            let last = hints.len() - 1;
            hints.swap(0, last);
        }
        self.remaining = hints;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(is_resident_session: bool, keys: Vec<(&'static str, &'static str)>) -> FeatureHintContext {
        FeatureHintContext {
            get_keybinding: Box::new(move |action| {
                keys.iter().find(|(key, _)| *key == action).map(|(_, value)| (*value).to_string())
            }),
            is_resident_session,
        }
    }

    #[test]
    fn hints_without_keybindings_are_skipped() {
        let context = context(false, vec![]);
        let mut deck = FeatureHintDeck::new(Box::new(|| 0.0));
        let mut ids = Vec::new();
        while let Some(hint) = deck.next(&context) {
            ids.push(hint.id);
            if ids.len() > FEATURE_HINTS.len() {
                break;
            }
        }
        assert!(!ids.contains(&"prompt-stash".to_string()));
        assert!(!ids.contains(&"agents-view".to_string()));
        assert!(!ids.contains(&"background-running".to_string()));
        assert!(ids.contains(&"side-question".to_string()));
        assert_eq!(ids.len(), FEATURE_HINTS.len() - 3);
    }

    #[test]
    fn deck_does_not_repeat_previous_id_at_the_front_of_the_next_refill() {
        // random() == 0.0 leaves the shuffle as a single pass that moves each
        // element to position 0, so the previous tail lands at index 0.
        let context = context(true, vec![("app.prompt.stash", "ctrl+s")]);
        let mut deck = FeatureHintDeck::new(Box::new(|| 0.0));
        let first = deck.next(&context).expect("first hint");
        assert_eq!(first.id, "side-question");
    }
}
