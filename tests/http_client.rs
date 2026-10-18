#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;

use std::time::Duration;

use mosaik_ki::KiError;
use mosaik_ki::client::{ChatBackend, HttpChat, Reply, ToolDef, parse_reply};
use mosaik_ki::http::{self, HttpError};
use serde_json::json;

#[test]
fn reads_content_length_response() {
    let (port, h) =
        common::one_shot_server(common::json_response(200, r#"{"ok":true}"#), Duration::ZERO);
    let r = http::request(port, "GET", "/health", None, Duration::from_secs(2)).unwrap();
    assert_eq!((r.status, r.body.as_str()), (200, r#"{"ok":true}"#));
    assert!(h.join().unwrap().starts_with("GET /health HTTP/1.1\r\n"));
}

#[test]
fn reads_chunked_response() {
    let raw = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n6\r\npedia!\r\n0\r\n\r\n".to_string();
    let (port, _h) = common::one_shot_server(raw, Duration::ZERO);
    let r = http::request(port, "GET", "/", None, Duration::from_secs(2)).unwrap();
    assert_eq!(r.body, "Wikipedia!");
}

#[test]
fn refused_when_nobody_listens() {
    let port = common::free_port();
    let e = http::request(port, "GET", "/", None, Duration::from_secs(1)).unwrap_err();
    assert_eq!(e, HttpError::Refused);
}

#[test]
fn chat_times_out_and_reports_timeout() {
    let (port, _h) =
        common::one_shot_server(common::json_response(200, "{}"), Duration::from_secs(3));
    let mut chat = HttpChat {
        port,
        timeout: Duration::from_millis(500),
        max_tokens: 16,
    };
    let e = chat
        .chat(&[json!({"role":"user","content":"hi"})], &[])
        .unwrap_err();
    assert_eq!(e, KiError::Timeout);
}

#[test]
fn chat_sends_utf8_and_tools_and_parses_text() {
    let body = r#"{"choices":[{"message":{"role":"assistant","content":"  Grüße 👋 "}}]}"#;
    let (port, h) = common::one_shot_server(common::json_response(200, body), Duration::ZERO);
    let tools = [ToolDef {
        name: "cell.set".into(),
        description: "Zelle setzen".into(),
        parameters: json!({"type":"object"}),
    }];
    let reply = HttpChat::new(port)
        .chat(&[json!({"role":"user","content":"Größe: 5 €"})], &tools)
        .unwrap();
    assert_eq!(reply, Reply::Text("Grüße 👋".into()));
    let req = h.join().unwrap();
    assert!(req.starts_with("POST /v1/chat/completions HTTP/1.1"));
    assert!(req.contains("Größe: 5 €"));
    assert!(req.contains(r#""name":"cell.set""#));
}

#[test]
fn parses_tool_calls_with_string_arguments() {
    let body = r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[
        {"id":"c1","type":"function","function":{"name":"cell.set","arguments":"{\"cell\":\"B2\",\"input\":\"=SUM(A1:A3)\"}"}}]}}]}"#;
    match parse_reply(body).unwrap() {
        Reply::ToolCalls { calls, raw } => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].id, "c1");
            assert_eq!(calls[0].name, "cell.set");
            assert_eq!(
                calls[0].arguments,
                json!({"cell":"B2","input":"=SUM(A1:A3)"})
            );
            assert_eq!(raw["role"], "assistant");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn empty_or_garbage_is_bad_response() {
    assert!(matches!(parse_reply("nope"), Err(KiError::BadResponse(_))));
    let empty = r#"{"choices":[{"message":{"role":"assistant","content":"   "}}]}"#;
    assert!(matches!(parse_reply(empty), Err(KiError::BadResponse(_))));
}

fn raw_server(reply: &'static [u8]) -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            use std::io::Write;
            let _ = s.write_all(reply);
        }
    });
    port
}

#[test]
fn complete_response_survives_reset_from_unread_request() {
    let port = raw_server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello world",
    );
    let r = http::request(
        port,
        "POST",
        "/",
        Some(&"x".repeat(50_000)),
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!((r.status, r.body.as_str()), (200, "hello world"));
}

#[test]
fn truncated_body_is_malformed() {
    let port = raw_server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n0123456789",
    );
    let e = http::request(port, "GET", "/", None, Duration::from_secs(2)).unwrap_err();
    assert!(matches!(e, HttpError::Malformed(_)), "{e:?}");
}

#[test]
fn dropped_connection_without_reply_is_refused() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || drop(listener.accept()));
    let r = http::request(
        port,
        "POST",
        "/",
        Some(&"x".repeat(50_000)),
        Duration::from_secs(2),
    );
    assert_eq!(r, Err(HttpError::Refused));
}
