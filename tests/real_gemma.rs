#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Läuft nur mit `cargo test --test real_gemma -- --ignored --nocapture` und installiertem Modell.
use std::time::{Duration, Instant};

use mosaik_ki::actions::{self, SHEET_ACTIONS, TEXT_ACTIONS};
use mosaik_ki::agent::{self, KiTools};
use mosaik_ki::client::{ChatBackend, HttpChat, Reply, ToolDef};
use mosaik_ki::{KiConfig, default_config_path, server};
use serde_json::{Value, json};

fn port() -> u16 {
    let path = default_config_path().unwrap();
    let mut cfg = KiConfig::load(&path).expect("mosaik-ki.json fehlt");
    let t = Instant::now();
    let port = server::ensure_running(&mut cfg, Some(&path), Duration::from_secs(300)).unwrap();
    println!("Dienst bereit nach {:?}", t.elapsed());
    port
}

fn ask(port: u16, action: &actions::QuickAction, input: &str) -> String {
    let t = Instant::now();
    let reply = HttpChat::new(port)
        .chat(&actions::build_messages(action, input).unwrap(), &[])
        .unwrap();
    let Reply::Text(text) = reply else {
        panic!("Werkzeugaufruf statt Text")
    };
    let out = actions::postprocess(action.apply, &text);
    println!("[{}] {:?} → {out:?}", action.id, t.elapsed());
    out
}

#[test]
#[ignore]
fn quick_actions_work_with_gemma() {
    let port = port();
    let fixed = ask(
        port,
        &TEXT_ACTIONS[0],
        "Das ist ein Tset mit einem Feler, das wir korigieren wollen.",
    );
    assert!(
        fixed.contains("Test") && fixed.contains("Fehler"),
        "{fixed}"
    );
    let formula = ask(
        port,
        &SHEET_ACTIONS[0],
        "Durchschnitt der Werte in B2 bis B20",
    );
    assert!(formula.starts_with("=AVERAGE("), "{formula}");
}

struct Sheet(Vec<Value>);
impl KiTools for Sheet {
    fn definitions(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "cell_set".into(),
            description: "Schreibt einen Wert oder eine Formel in eine Zelle.".into(),
            parameters: json!({"type":"object","properties":{"cell":{"type":"string"},"input":{"type":"string"}},"required":["cell","input"]}),
        }]
    }
    fn call(&mut self, _name: &str, args: &Value) -> Result<Value, String> {
        self.0.push(args.clone());
        Ok(json!({"ok": true}))
    }
}

#[test]
#[ignore]
fn gemma_calls_tools() {
    let port = port();
    let mut chat = HttpChat::new(port);
    let mut sheet = Sheet(vec![]);
    let out = agent::run_blocking(
        &mut chat,
        &mut sheet,
        actions::SYSTEM_PROMPT,
        "Schreibe in Zelle A1 das Wort Umsatz.",
    )
    .unwrap();
    println!("Werkzeugaufrufe: {:?}\nAntwort: {out}", sheet.0);
    assert!(sheet.0.iter().any(|a| a["cell"] == "A1"), "{:?}", sheet.0);
}
