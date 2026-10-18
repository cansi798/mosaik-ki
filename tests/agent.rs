#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::collections::VecDeque;

use mosaik_ki::KiError;
use mosaik_ki::agent::{self, KiTools};
use mosaik_ki::client::{ChatBackend, Reply, ToolCall, ToolDef};
use serde_json::{Value, json};

struct Scripted(VecDeque<Reply>, Vec<Vec<Value>>);
impl ChatBackend for Scripted {
    fn chat(&mut self, messages: &[Value], _tools: &[ToolDef]) -> Result<Reply, KiError> {
        self.1.push(messages.to_vec());
        self.0
            .pop_front()
            .ok_or(KiError::BadResponse("Skript leer".into()))
    }
}

#[derive(Default)]
struct Cells {
    set: Vec<Value>,
    fail_times: usize,
}
impl KiTools for Cells {
    fn definitions(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "cell.set".into(),
            description: "Zelle setzen".into(),
            parameters: json!({"type":"object"}),
        }]
    }
    fn call(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        if self.fail_times > 0 {
            self.fail_times -= 1;
            return Err("ungültiger Bereich".into());
        }
        assert_eq!(name, "cell.set");
        self.set.push(args.clone());
        Ok(json!({"ok": true}))
    }
}

fn call(args: Value) -> Reply {
    Reply::ToolCalls {
        calls: vec![ToolCall {
            id: "c1".into(),
            name: "cell.set".into(),
            arguments: args,
        }],
        raw: json!({"role":"assistant","tool_calls":[]}),
    }
}

#[test]
fn executes_tool_then_returns_final_text() {
    let mut chat = Scripted(
        VecDeque::from([call(json!({"cell":"A1"})), Reply::Text("Fertig".into())]),
        vec![],
    );
    let mut tools = Cells::default();
    let out = agent::run_blocking(&mut chat, &mut tools, "sys", "Setze A1").unwrap();
    assert_eq!(out, "Fertig");
    assert_eq!(tools.set, vec![json!({"cell":"A1"})]);
    let second = &chat.1[1];
    assert_eq!(second.last().unwrap()["role"], "tool");
    assert_eq!(second.last().unwrap()["tool_call_id"], "c1");
}

#[test]
fn first_tool_error_is_reported_back_once() {
    let mut chat = Scripted(
        VecDeque::from([
            call(json!({"cell":"ZZ"})),
            call(json!({"cell":"A1"})),
            Reply::Text("ok".into()),
        ]),
        vec![],
    );
    let mut tools = Cells {
        fail_times: 1,
        ..Default::default()
    };
    assert_eq!(
        agent::run_blocking(&mut chat, &mut tools, "s", "u").unwrap(),
        "ok"
    );
    let fed_back = chat.1[1].last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(fed_back.contains("ungültiger Bereich"));
}

#[test]
fn second_tool_error_fails_the_run() {
    let mut chat = Scripted(VecDeque::from([call(json!({})), call(json!({}))]), vec![]);
    let mut tools = Cells {
        fail_times: 2,
        ..Default::default()
    };
    let e = agent::run_blocking(&mut chat, &mut tools, "s", "u").unwrap_err();
    assert_eq!(e, KiError::ToolFailed("ungültiger Bereich".into()));
}

#[test]
fn stops_after_max_steps() {
    let replies: VecDeque<Reply> = (0..20).map(|_| call(json!({}))).collect();
    let mut chat = Scripted(replies, vec![]);
    let mut tools = Cells::default();
    let e = agent::run_blocking(&mut chat, &mut tools, "s", "u").unwrap_err();
    assert!(matches!(e, KiError::ToolFailed(_)));
    assert!(tools.set.len() < agent::MAX_STEPS);
}

#[test]
fn empty_text_after_tool_round_is_done() {
    let mut chat = Scripted(
        VecDeque::from([call(json!({"cell":"A1"})), Reply::Text(String::new())]),
        vec![],
    );
    let mut tools = Cells::default();
    assert_eq!(
        agent::run_blocking(&mut chat, &mut tools, "s", "u").unwrap(),
        "Erledigt."
    );
}

#[test]
fn empty_text_first_is_bad_response() {
    let mut chat = Scripted(VecDeque::from([Reply::Text("  ".into())]), vec![]);
    let mut tools = Cells::default();
    assert!(matches!(
        agent::run_blocking(&mut chat, &mut tools, "s", "u"),
        Err(KiError::BadResponse(_))
    ));
}
