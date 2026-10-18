//! KI-Seitenleiste für egui-Apps. HTTP läuft in Hintergrund-Threads, alles andere auf dem UI-Thread.
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Duration;

use serde_json::Value;

use crate::actions::{self, Apply, QuickAction};
use crate::agent::{AgentRun, KiTools, Step};
use crate::client::{ChatBackend, HttpChat, Reply, ToolDef};
use crate::{KiConfig, KiError, default_config_path, server};

/// Was die App der Seitenleiste zur Verfügung stellt.
pub trait KiHost {
    /// Markierter Text bzw. Bereichsinhalt; ohne Markierung das ganze Dokument.
    fn input_text(&self) -> String;
    fn apply(&mut self, apply: Apply, text: &str) -> Result<(), String>;
    fn tools(&mut self) -> &mut dyn KiTools;
    fn begin_undo_group(&mut self);
    /// `keep == false`: alle Schritte seit `begin_undo_group` zurücknehmen.
    fn end_undo_group(&mut self, keep: bool);
    /// Öffnet „Mosaik einrichten“ bei der Stufenwahl.
    fn open_setup(&mut self);
}

#[derive(Debug)]
enum Job {
    Quick(&'static QuickAction),
    Agent(AgentRun),
}

#[derive(Debug, Clone, Copy)]
enum Click {
    Cancel,
    Accept,
    Dismiss,
}

#[derive(Debug)]
enum State {
    Idle,
    Busy {
        rx: Receiver<Result<Reply, KiError>>,
        job: Job,
    },
    Preview {
        text: String,
        apply: Apply,
    },
    Message(String),
}

pub struct KiPanel {
    actions: &'static [QuickAction],
    instruction: String,
    state: State,
    ready: Option<bool>,
}

impl KiPanel {
    pub fn new(actions: &'static [QuickAction]) -> Self {
        Self {
            actions,
            instruction: String::new(),
            state: State::Idle,
            ready: None,
        }
    }

    /// Prüft erneut, ob die KI installiert ist (z. B. nach „KI-Stufe ändern“).
    pub fn refresh(&mut self) {
        self.ready = None;
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.state, State::Busy { .. })
    }

    fn ready(&mut self) -> bool {
        *self.ready.get_or_insert_with(|| {
            default_config_path()
                .and_then(|p| KiConfig::load(&p))
                .is_some_and(|c| c.is_ready())
        })
    }

    pub fn show(&mut self, ui: &mut egui::Ui, host: &mut dyn KiHost) {
        self.poll(host);
        ui.heading("KI");
        if !self.ready() {
            ui.label(KiError::NotInstalled.user_message());
            if ui.button("KI-Stufe ändern …").clicked() {
                host.open_setup();
                self.refresh();
            }
            return;
        }
        let idle = matches!(self.state, State::Idle);
        ui.add_space(4.0);
        for action in self.actions {
            if ui
                .add_enabled(idle, egui::Button::new(action.label))
                .clicked()
            {
                self.start_quick(action, &host.input_text());
            }
        }
        ui.separator();
        ui.label("Eigene Anweisung");
        ui.add_enabled(
            idle,
            egui::TextEdit::multiline(&mut self.instruction)
                .desired_rows(3)
                .hint_text("z. B. „Mach aus der Liste eine Tabelle“"),
        );
        if ui
            .add_enabled(
                idle && !self.instruction.trim().is_empty(),
                egui::Button::new("Ausführen"),
            )
            .clicked()
        {
            self.start_agent(host);
        }
        ui.separator();
        // Klicks erst sammeln, dann ausführen: während `self.state` ausgeliehen ist, darf `self` nicht verändert werden.
        let mut click = None;
        match &self.state {
            State::Idle => {}
            State::Busy { .. } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("KI arbeitet …");
                });
                ui.ctx().request_repaint_after(Duration::from_millis(100));
                if ui.button("Abbrechen").clicked() {
                    click = Some(Click::Cancel);
                }
            }
            State::Preview { text, apply } => {
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .show(ui, |ui| {
                        ui.label(text.as_str());
                    });
                ui.horizontal(|ui| {
                    if *apply != Apply::ShowOnly && ui.button("Übernehmen").clicked() {
                        click = Some(Click::Accept);
                    }
                    if ui
                        .button(if *apply == Apply::ShowOnly {
                            "Schließen"
                        } else {
                            "Verwerfen"
                        })
                        .clicked()
                    {
                        click = Some(Click::Dismiss);
                    }
                    if ui.button("Kopieren").clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                });
            }
            State::Message(msg) => {
                ui.label(msg.as_str());
                if ui.button("OK").clicked() {
                    click = Some(Click::Dismiss);
                }
            }
        }
        match click {
            Some(Click::Cancel) => self.cancel(host),
            Some(Click::Accept) => self.accept_preview(host),
            Some(Click::Dismiss) => self.state = State::Idle,
            None => {}
        }
    }

    fn start_quick(&mut self, action: &'static QuickAction, input: &str) {
        match actions::build_messages(action, input) {
            Ok(messages) => {
                let rx = spawn_chat(messages, Vec::new());
                self.state = State::Busy {
                    rx,
                    job: Job::Quick(action),
                };
            }
            Err(e) => self.state = State::Message(e.user_message()),
        }
    }

    fn start_agent(&mut self, host: &mut dyn KiHost) {
        let user = format!(
            "{}\n\nAktueller Inhalt:\n<<<\n{}\n>>>",
            self.instruction.trim(),
            host.input_text()
        );
        let run = AgentRun::new(actions::SYSTEM_PROMPT, &user);
        let defs = host.tools().definitions();
        host.begin_undo_group();
        let rx = spawn_chat(run.messages().to_vec(), defs);
        self.state = State::Busy {
            rx,
            job: Job::Agent(run),
        };
    }

    fn cancel(&mut self, host: &mut dyn KiHost) {
        if let State::Busy {
            job: Job::Agent(_), ..
        } = self.state
        {
            host.end_undo_group(false);
        }
        self.state = State::Idle;
    }

    fn poll(&mut self, host: &mut dyn KiHost) {
        let State::Busy { rx, .. } = &self.state else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(KiError::Unavailable("abgebrochen".into())),
        };
        let State::Busy { job, .. } = std::mem::replace(&mut self.state, State::Idle) else {
            return;
        };
        self.state = match (job, result) {
            (Job::Quick(action), Ok(Reply::Text(t))) => State::Preview {
                text: actions::postprocess(action.apply, &t),
                apply: action.apply,
            },
            (Job::Quick(_), Ok(Reply::ToolCalls { .. })) => {
                State::Message(KiError::BadResponse("unerwartet".into()).user_message())
            }
            (Job::Quick(_), Err(e)) => State::Message(e.user_message()),
            (Job::Agent(_), Err(e)) => {
                host.end_undo_group(false);
                State::Message(e.user_message())
            }
            (Job::Agent(mut run), Ok(reply)) => match run.on_reply(reply, host.tools()) {
                Step::NeedChat => {
                    let defs = host.tools().definitions();
                    let rx = spawn_chat(run.messages().to_vec(), defs);
                    State::Busy {
                        rx,
                        job: Job::Agent(run),
                    }
                }
                Step::Done(text) => {
                    host.end_undo_group(true);
                    self.instruction.clear();
                    State::Message(actions::postprocess(Apply::ShowOnly, &text))
                }
                Step::Failed(e) => {
                    host.end_undo_group(false);
                    State::Message(e.user_message())
                }
            },
        };
    }

    fn accept_preview(&mut self, host: &mut dyn KiHost) {
        if let State::Preview { text, apply } = &self.state {
            self.state = match host.apply(*apply, text) {
                Ok(()) => State::Idle,
                Err(why) => State::Message(KiError::ToolFailed(why).user_message()),
            };
        }
    }
}

/// Startet den Dienst (falls nötig) und stellt eine Chat-Anfrage im Hintergrund.
fn spawn_chat(messages: Vec<Value>, tools: Vec<ToolDef>) -> Receiver<Result<Reply, KiError>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let result = (|| {
            let path = default_config_path().ok_or(KiError::NotInstalled)?;
            let mut cfg = KiConfig::load(&path).ok_or(KiError::NotInstalled)?;
            let port = server::ensure_running(&mut cfg, Some(&path), Duration::from_secs(180))?;
            HttpChat::new(port).chat(&messages, &tools)
        })();
        let _ = tx.send(result);
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::TEXT_ACTIONS;
    use crate::client::ToolDef;
    use serde_json::Value;

    struct NoTools;
    impl KiTools for NoTools {
        fn definitions(&self) -> Vec<ToolDef> {
            vec![]
        }
        fn call(&mut self, _: &str, _: &Value) -> Result<Value, String> {
            Err("keine".into())
        }
    }

    #[derive(Default)]
    struct Host {
        applied: Vec<String>,
        groups: Vec<bool>,
        tools: Option<NoTools>,
    }
    impl KiHost for Host {
        fn input_text(&self) -> String {
            "Hallo".into()
        }
        fn apply(&mut self, _: Apply, text: &str) -> Result<(), String> {
            self.applied.push(text.into());
            Ok(())
        }
        fn tools(&mut self) -> &mut dyn KiTools {
            self.tools.get_or_insert(NoTools)
        }
        fn begin_undo_group(&mut self) {}
        fn end_undo_group(&mut self, keep: bool) {
            self.groups.push(keep);
        }
        fn open_setup(&mut self) {}
    }

    fn frame(panel: &mut KiPanel, host: &mut Host) {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |ui| panel.show(ui, host))
            .drop_without_applying_deltas();
    }

    #[test]
    fn renders_not_installed_without_panicking() {
        let mut panel = KiPanel::new(&TEXT_ACTIONS);
        panel.ready = Some(false);
        let mut host = Host::default();
        frame(&mut panel, &mut host);
        assert!(!panel.is_busy());
    }

    #[test]
    fn quick_result_becomes_preview_and_apply_uses_host() {
        let mut panel = KiPanel::new(&TEXT_ACTIONS);
        panel.ready = Some(true);
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok(crate::client::Reply::Text(
            "```\nKorrigiert.\n```".into(),
        )))
        .unwrap();
        panel.state = State::Busy {
            rx,
            job: Job::Quick(&TEXT_ACTIONS[0]),
        };
        let mut host = Host::default();
        panel.poll(&mut host);
        match &panel.state {
            State::Preview { text, .. } => assert_eq!(text, "Korrigiert."),
            other => panic!("{other:?}"),
        }
        panel.accept_preview(&mut host);
        assert_eq!(host.applied, vec!["Korrigiert.".to_string()]);
    }

    #[test]
    fn agent_failure_rolls_back_undo_group() {
        let mut panel = KiPanel::new(&TEXT_ACTIONS);
        panel.ready = Some(true);
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Err(KiError::Timeout)).unwrap();
        panel.state = State::Busy {
            rx,
            job: Job::Agent(AgentRun::new("s", "u")),
        };
        let mut host = Host::default();
        panel.poll(&mut host);
        assert_eq!(host.groups, vec![false]);
        assert!(matches!(panel.state, State::Message(_)));
    }
}
