//! KI-Seitenleiste für egui-Apps. HTTP läuft in Hintergrund-Threads, alles andere auf dem UI-Thread.
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Duration;

use serde_json::Value;

use crate::actions::{self, Apply, QuickAction};
use crate::agent::{AgentRun, KiTools, Step};
use crate::client::{ChatBackend, HttpChat, Reply, ToolDef};
use crate::{KiConfig, KiError, default_config_path, server};

/// Was die App der Seitenleiste zur Verfügung stellt.
///
/// Vertrag zwischen App und Seitenleiste:
///
/// 1. Solange [`KiPanel::locks_document`] `true` liefert (KI arbeitet oder eine Vorschau wartet auf
///    „Übernehmen“), macht die App das Dokument schreibgeschützt und ignoriert Änderungen und
///    Auswahlwechsel. Sonst würde die Antwort auf veralteten Inhalt angewendet.
/// 2. Nach dem Schließen von „Mosaik einrichten“ (`open_setup`) ruft die App [`KiPanel::refresh`] auf.
///    Mit dem Feature `install` installiert die Seitenleiste die KI selbst; `open_setup` wird dann
///    nicht mehr aufgerufen.
/// 3. `end_undo_group(false)` stellt exakt den Zustand wieder her, den `begin_undo_group` festgehalten hat.
pub trait KiHost {
    /// Markierter Text bzw. Bereichsinhalt; ohne Markierung das ganze Dokument.
    fn input_text(&self) -> String;
    fn apply(&mut self, apply: Apply, text: &str) -> Result<(), String>;
    fn tools(&mut self) -> &mut dyn KiTools;
    fn begin_undo_group(&mut self);
    /// `keep == false`: den Zustand von `begin_undo_group` wiederherstellen (alle Schritte zurücknehmen).
    fn end_undo_group(&mut self, keep: bool);
    /// Öffnet „Mosaik einrichten“ bei der Stufenwahl. Nur ohne Feature `install` nötig: mit ihm
    /// zeigt die Seitenleiste die Stufenwahl selbst.
    fn open_setup(&mut self) {}
}

/// Antwortlänge im Agentenbetrieb (kurze Werkzeugaufrufe und Abschlusstext).
const AGENT_MAX_TOKENS: u32 = 1024;

#[derive(Debug)]
enum Job {
    Quick(&'static QuickAction),
    Agent(AgentRun),
}

#[derive(Debug, Clone, Copy)]
enum Click {
    ChangeTier,
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
    #[cfg(feature = "install")]
    setup: Option<crate::setup_view::SetupView>,
}

impl KiPanel {
    pub fn new(actions: &'static [QuickAction]) -> Self {
        Self {
            actions,
            instruction: String::new(),
            state: State::Idle,
            ready: None,
            #[cfg(feature = "install")]
            setup: None,
        }
    }

    /// Öffnet die Stufenwahl in der Seitenleiste (z. B. aus dem Hilfe-Menü „KI-Stufe ändern …“).
    /// Während die KI arbeitet, passiert nichts.
    #[cfg(feature = "install")]
    pub fn open_tier_choice(&mut self) {
        if matches!(self.state, State::Idle | State::Message(_)) && self.setup.is_none() {
            self.state = State::Idle;
            let closable = self.ready();
            self.setup = Some(crate::setup_view::SetupView::open(closable));
        }
    }

    /// `true`, solange die KI installiert oder entfernt wird.
    #[cfg(feature = "install")]
    pub fn is_installing(&self) -> bool {
        self.setup.as_ref().is_some_and(|s| s.is_busy())
    }

    /// Knopf „KI-Stufe ändern …“: mit Feature `install` die eigene Stufenwahl, sonst „Mosaik einrichten“.
    fn change_tier(&mut self, host: &mut dyn KiHost) {
        #[cfg(feature = "install")]
        {
            let _ = host;
            self.open_tier_choice();
        }
        #[cfg(not(feature = "install"))]
        {
            host.open_setup();
            self.refresh();
        }
    }

    /// Zeichnet die Stufenwahl, falls offen oder nötig. `true` = sonst nichts zeichnen.
    #[cfg(feature = "install")]
    fn show_setup(&mut self, ui: &mut egui::Ui) -> bool {
        use crate::setup_view::{Outcome, SetupView};
        if self.setup.is_none() && matches!(self.state, State::Idle) && !self.ready() {
            self.setup = Some(SetupView::open(false));
        }
        let Some(view) = self.setup.as_mut() else {
            return false;
        };
        match view.show(ui) {
            Outcome::Open => true,
            Outcome::Close => {
                self.setup = None;
                false
            }
            Outcome::Finished => {
                self.setup = None;
                self.refresh();
                ui.ctx().request_repaint();
                true
            }
        }
    }

    /// Prüft erneut, ob die KI installiert ist (z. B. nach „KI-Stufe ändern“).
    pub fn refresh(&mut self) {
        self.ready = None;
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.state, State::Busy { .. })
    }

    /// `true`, solange die App das Dokument nicht ändern darf: während die KI arbeitet und
    /// während eine übernehmbare Vorschau offen ist.
    pub fn locks_document(&self) -> bool {
        match &self.state {
            State::Busy { .. } => true,
            State::Preview { apply, .. } => *apply != Apply::ShowOnly,
            State::Idle | State::Message(_) => false,
        }
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
        #[cfg(feature = "install")]
        if self.show_setup(ui) {
            return;
        }
        // Eine laufende Arbeit, Vorschau oder Meldung bleibt auch sichtbar, wenn `ready` umspringt.
        if !self.ready() && matches!(self.state, State::Idle) {
            ui.label(KiError::NotInstalled.user_message());
            if ui.button("KI-Stufe ändern …").clicked() {
                self.change_tier(host);
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
                let input = host.input_text();
                self.start_quick(action, &input);
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
        if matches!(self.state, State::Idle) {
            ui.add_space(8.0);
            if ui.small_button("KI-Stufe ändern …").clicked() {
                click = Some(Click::ChangeTier);
            }
        }
        match click {
            Some(Click::ChangeTier) => self.change_tier(host),
            Some(Click::Cancel) => self.cancel(host),
            Some(Click::Accept) => self.accept_preview(host),
            Some(Click::Dismiss) => self.state = State::Idle,
            None => {}
        }
    }

    /// Bei „Formel aus Beschreibung“ dient das Feld „Eigene Anweisung“ als Beschreibung.
    fn quick_messages(&self, action: &QuickAction, input: &str) -> Result<Vec<Value>, KiError> {
        actions::build_messages_with(action, input, &self.instruction)
    }

    fn start_quick(&mut self, action: &'static QuickAction, input: &str) {
        match self.quick_messages(action, input) {
            Ok(messages) => {
                let rx = spawn_chat(messages, Vec::new(), actions::max_tokens_for(input));
                self.state = State::Busy {
                    rx,
                    job: Job::Quick(action),
                };
            }
            Err(e) => self.state = State::Message(e.user_message()),
        }
    }

    fn start_agent(&mut self, host: &mut dyn KiHost) {
        let instruction = self.instruction.trim().to_string();
        if instruction.is_empty() {
            return;
        }
        let user = format!(
            "{instruction}\n\nAktueller Inhalt:\n<<<\n{}\n>>>",
            host.input_text()
        );
        let chars = user.chars().count();
        if chars > actions::MAX_INPUT_CHARS {
            self.state = State::Message(
                KiError::TooLong {
                    chars,
                    max: actions::MAX_INPUT_CHARS,
                }
                .user_message(),
            );
            return;
        }
        let run = AgentRun::new(actions::SYSTEM_PROMPT, &user);
        let defs = host.tools().definitions();
        host.begin_undo_group();
        let rx = spawn_chat(run.messages().to_vec(), defs, AGENT_MAX_TOKENS);
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
            (Job::Quick(action), Ok(Reply::Text(t))) => {
                let text = actions::postprocess(action.apply, &t);
                if text.is_empty() || text == "=" {
                    State::Message(KiError::BadResponse("leere Antwort".into()).user_message())
                } else {
                    State::Preview {
                        text,
                        apply: action.apply,
                    }
                }
            }
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
                    let rx = spawn_chat(run.messages().to_vec(), defs, AGENT_MAX_TOKENS);
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
fn spawn_chat(
    messages: Vec<Value>,
    tools: Vec<ToolDef>,
    max_tokens: u32,
) -> Receiver<Result<Reply, KiError>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let result = (|| {
            let path = default_config_path().ok_or(KiError::NotInstalled)?;
            let mut cfg = KiConfig::load(&path).ok_or(KiError::NotInstalled)?;
            let port = server::ensure_running(&mut cfg, Some(&path), Duration::from_secs(180))?;
            let mut chat = HttpChat::new(port);
            chat.max_tokens = max_tokens;
            chat.chat(&messages, &tools)
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
        input: String,
        begins: usize,
    }
    impl KiHost for Host {
        fn input_text(&self) -> String {
            if self.input.is_empty() {
                "Hallo".into()
            } else {
                self.input.clone()
            }
        }
        fn apply(&mut self, _: Apply, text: &str) -> Result<(), String> {
            self.applied.push(text.into());
            Ok(())
        }
        fn tools(&mut self) -> &mut dyn KiTools {
            self.tools.get_or_insert(NoTools)
        }
        fn begin_undo_group(&mut self) {
            self.begins += 1;
        }
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
    fn empty_quick_result_becomes_message() {
        let mut panel = KiPanel::new(&TEXT_ACTIONS);
        panel.ready = Some(true);
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok(crate::client::Reply::Text("  ".into())))
            .unwrap();
        panel.state = State::Busy {
            rx,
            job: Job::Quick(&TEXT_ACTIONS[0]),
        };
        let mut host = Host::default();
        panel.poll(&mut host);
        match &panel.state {
            State::Message(m) => {
                assert_eq!(*m, KiError::BadResponse("x".into()).user_message());
            }
            other => panic!("{other:?}"),
        }
    }

    fn busy_state(job: Job) -> State {
        let (_tx, rx) = std::sync::mpsc::channel();
        State::Busy { rx, job }
    }

    #[test]
    fn locks_document_while_busy_or_applicable_preview() {
        let mut panel = KiPanel::new(&TEXT_ACTIONS);
        assert!(!panel.locks_document());
        panel.state = busy_state(Job::Quick(&TEXT_ACTIONS[0]));
        assert!(panel.locks_document());
        for (apply, locked) in [
            (Apply::ReplaceSelection, true),
            (Apply::InsertFormula, true),
            (Apply::ShowOnly, false),
        ] {
            panel.state = State::Preview {
                text: "x".into(),
                apply,
            };
            assert_eq!(panel.locks_document(), locked, "{apply:?}");
        }
        panel.state = State::Message("m".into());
        assert!(!panel.locks_document());
    }

    #[test]
    fn agent_with_too_long_input_is_rejected_without_undo_group() {
        let mut panel = KiPanel::new(&TEXT_ACTIONS);
        panel.ready = Some(true);
        panel.instruction = "Mach etwas".into();
        let mut host = Host {
            input: "ä".repeat(actions::MAX_INPUT_CHARS + 1),
            ..Host::default()
        };
        panel.start_agent(&mut host);
        match &panel.state {
            State::Message(m) => {
                assert_eq!(*m, KiError::TooLong { chars: 0, max: 0 }.user_message())
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(host.begins, 0);
        assert!(host.groups.is_empty());
    }

    #[test]
    fn formula_action_sends_instruction_as_description() {
        let formel = &crate::actions::SHEET_ACTIONS[0];
        let mut panel = KiPanel::new(&crate::actions::SHEET_ACTIONS);
        panel.ready = Some(true);
        panel.instruction = "  ".into();
        panel.start_quick(formel, "A1: 5");
        match &panel.state {
            State::Message(m) => {
                assert_eq!(m, "Bitte beschreiben Sie, was die Formel berechnen soll.")
            }
            other => panic!("{other:?}"),
        }
        panel.instruction = "Summe von A1 bis A3".into();
        let messages = panel.quick_messages(formel, "A1: 5").unwrap();
        let user = messages[1]["content"].as_str().unwrap();
        assert!(user.contains("Beschreibung: Summe von A1 bis A3"), "{user}");
        assert!(user.contains("A1: 5"), "{user}");
        // Andere Aktionen ignorieren das Feld.
        let erklaeren = &crate::actions::SHEET_ACTIONS[1];
        let user = panel.quick_messages(erklaeren, "A1: 5").unwrap()[1]["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!user.contains("Summe von A1"), "{user}");
    }

    #[test]
    fn agent_with_empty_instruction_does_nothing() {
        let mut panel = KiPanel::new(&TEXT_ACTIONS);
        panel.instruction = "  ".into();
        let mut host = Host::default();
        panel.start_agent(&mut host);
        assert!(matches!(panel.state, State::Idle));
        assert_eq!(host.begins, 0);
    }

    #[test]
    fn fence_only_or_proseless_formula_answer_becomes_message() {
        for (action, answer) in [
            (&TEXT_ACTIONS[0], "```\n```"),
            (&crate::actions::SHEET_ACTIONS[0], "Das geht leider nicht."),
            (&crate::actions::SHEET_ACTIONS[0], "="),
        ] {
            let mut panel = KiPanel::new(&TEXT_ACTIONS);
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(Ok(crate::client::Reply::Text(answer.into())))
                .unwrap();
            panel.state = State::Busy {
                rx,
                job: Job::Quick(action),
            };
            panel.poll(&mut Host::default());
            assert!(matches!(panel.state, State::Message(_)), "{answer}");
        }
    }

    #[test]
    fn busy_run_stays_visible_when_ready_flips_false() {
        let mut panel = KiPanel::new(&TEXT_ACTIONS);
        panel.ready = Some(false);
        panel.state = State::Message("Wichtig".into());
        let ctx = egui::Context::default();
        let mut host = Host::default();
        let mut texts = String::new();
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            panel.show(ui, &mut host);
        });
        for clipped in &out.shapes {
            texts.push_str(&format!("{:?}", clipped.shape));
        }
        out.drop_without_applying_deltas();
        assert!(texts.contains("Wichtig"), "{texts}");
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
