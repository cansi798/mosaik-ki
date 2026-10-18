//! Agenten-Schleife als Schrittmaschine: HTTP darf im Hintergrund laufen, Werkzeuge laufen auf dem
//! Thread der App (deren Zustand ist nicht `Send`).
use serde_json::{Value, json};

use crate::KiError;
use crate::client::{ChatBackend, Reply, ToolDef};

pub trait KiTools {
    fn definitions(&self) -> Vec<ToolDef>;
    fn call(&mut self, name: &str, args: &Value) -> Result<Value, String>;
}

pub const MAX_STEPS: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    NeedChat,
    Done(String),
    Failed(KiError),
}

#[derive(Debug, Clone)]
pub struct AgentRun {
    messages: Vec<Value>,
    steps: usize,
    retried: bool,
}

impl AgentRun {
    pub fn new(system: &str, user: &str) -> Self {
        Self {
            messages: vec![
                json!({"role": "system", "content": system}),
                json!({"role": "user", "content": user}),
            ],
            steps: 0,
            retried: false,
        }
    }

    pub fn messages(&self) -> &[Value] {
        &self.messages
    }

    pub fn on_reply(&mut self, reply: Reply, tools: &mut dyn KiTools) -> Step {
        self.steps += 1;
        match reply {
            Reply::Text(text) if text.trim().is_empty() => {
                if self.steps > 1 {
                    Step::Done("Erledigt.".into())
                } else {
                    Step::Failed(KiError::BadResponse("leere Antwort".into()))
                }
            }
            Reply::Text(text) => Step::Done(text),
            Reply::ToolCalls { calls, raw } => {
                if self.steps >= MAX_STEPS {
                    return Step::Failed(KiError::ToolFailed("zu viele Schritte".into()));
                }
                self.messages.push(raw);
                for call in calls {
                    let content = match tools.call(&call.name, &call.arguments) {
                        Ok(result) => result.to_string(),
                        Err(err) if !self.retried => {
                            self.retried = true;
                            format!("Fehler: {err}. Bitte korrigiere den Aufruf.")
                        }
                        Err(err) => return Step::Failed(KiError::ToolFailed(err)),
                    };
                    self.messages
                        .push(json!({"role": "tool", "tool_call_id": call.id, "content": content}));
                }
                Step::NeedChat
            }
        }
    }
}

/// Blockierender Durchlauf (Tests, Kommandozeile). Die Seitenleiste nutzt `AgentRun` direkt.
pub fn run_blocking(
    chat: &mut dyn ChatBackend,
    tools: &mut dyn KiTools,
    system: &str,
    user: &str,
) -> Result<String, KiError> {
    let defs = tools.definitions();
    let mut run = AgentRun::new(system, user);
    loop {
        let reply = chat.chat(run.messages(), &defs)?;
        match run.on_reply(reply, tools) {
            Step::NeedChat => continue,
            Step::Done(text) => return Ok(text),
            Step::Failed(err) => return Err(err),
        }
    }
}
