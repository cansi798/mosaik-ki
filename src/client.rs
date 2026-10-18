//! OpenAI-kompatible Chat-Anfragen an llama-server, inklusive Tool-Calling.
use std::time::Duration;

use serde_json::{Value, json};

use crate::KiError;
use crate::http::{self, HttpError};

/// Modellname, unter dem llama-server das Gemma-Modell anbietet (`--alias`).
pub const ALIAS: &str = "mosaik";

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Text(String),
    /// `raw` ist die Assistenten-Nachricht, die unverändert in den Verlauf gehört.
    ToolCalls {
        calls: Vec<ToolCall>,
        raw: Value,
    },
}

pub trait ChatBackend {
    fn chat(&mut self, messages: &[Value], tools: &[ToolDef]) -> Result<Reply, KiError>;
}

#[derive(Debug, Clone)]
pub struct HttpChat {
    pub port: u16,
    pub timeout: Duration,
    pub max_tokens: u32,
}

impl HttpChat {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            timeout: Duration::from_secs(60),
            max_tokens: 1024,
        }
    }
}

impl ChatBackend for HttpChat {
    fn chat(&mut self, messages: &[Value], tools: &[ToolDef]) -> Result<Reply, KiError> {
        let body = request_body(messages, tools, self.max_tokens).to_string();
        let resp = http::request(
            self.port,
            "POST",
            "/v1/chat/completions",
            Some(&body),
            self.timeout,
        )
        .map_err(|e| match e {
            HttpError::Timeout => KiError::Timeout,
            other => KiError::Unavailable(format!("{other:?}")),
        })?;
        if resp.status != 200 {
            let snippet: String = resp.body.chars().take(200).collect();
            return Err(KiError::BadResponse(format!(
                "Status {}: {snippet}",
                resp.status
            )));
        }
        parse_reply(&resp.body)
    }
}

pub fn request_body(messages: &[Value], tools: &[ToolDef], max_tokens: u32) -> Value {
    let mut body = json!({
        "model": ALIAS,
        "messages": messages,
        "temperature": 0.3,
        "max_tokens": max_tokens,
    });
    if !tools.is_empty() {
        let list: Vec<Value> = tools
            .iter()
            .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.parameters}}))
            .collect();
        body["tools"] = Value::Array(list);
    }
    body
}

pub fn parse_reply(body: &str) -> Result<Reply, KiError> {
    let v: Value = serde_json::from_str(body).map_err(|e| KiError::BadResponse(e.to_string()))?;
    let msg = v
        .pointer("/choices/0/message")
        .ok_or_else(|| KiError::BadResponse("keine choices".into()))?;
    if let Some(calls) = msg
        .get("tool_calls")
        .and_then(Value::as_array)
        .filter(|c| !c.is_empty())
    {
        let mut parsed = Vec::with_capacity(calls.len());
        for (i, c) in calls.iter().enumerate() {
            let name = c
                .pointer("/function/name")
                .and_then(Value::as_str)
                .ok_or_else(|| KiError::BadResponse("Werkzeugname fehlt".into()))?;
            let arguments = match c.pointer("/function/arguments") {
                Some(Value::String(s)) => {
                    serde_json::from_str(s).map_err(|e| KiError::BadResponse(e.to_string()))?
                }
                Some(other) => other.clone(),
                None => json!({}),
            };
            let id = c
                .get("id")
                .and_then(Value::as_str)
                .map_or_else(|| format!("call_{i}"), str::to_string);
            parsed.push(ToolCall {
                id,
                name: name.to_string(),
                arguments,
            });
        }
        return Ok(Reply::ToolCalls {
            calls: parsed,
            raw: msg.clone(),
        });
    }
    let text = msg
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if let Some(calls) = parse_native_calls(text) {
        let list: Vec<Value> = calls
            .iter()
            .map(|c| {
                json!({"id": c.id, "type": "function",
                    "function": {"name": c.name, "arguments": c.arguments.to_string()}})
            })
            .collect();
        return Ok(Reply::ToolCalls {
            calls,
            raw: json!({"role": "assistant", "content": null, "tool_calls": list}),
        });
    }
    Ok(Reply::Text(text.to_string()))
}

const QUOTE: &str = "<|\"|>";

/// Gemma-natives Aufrufformat, falls der Server es nicht selbst zerlegt hat:
/// `name{key:<|"|>wert<|"|>,zahl:3}`, ein- oder mehrfach. Nur wenn der ganze Text passt.
fn parse_native_calls(text: &str) -> Option<Vec<ToolCall>> {
    let mut rest = text.trim();
    let mut calls = Vec::new();
    while !rest.is_empty() {
        let name_len = rest
            .char_indices()
            .find(|&(i, c)| {
                !(c == '_'
                    || c.is_ascii_alphabetic()
                    || (i > 0 && (c.is_ascii_digit() || c == '.')))
            })
            .map_or(rest.len(), |(i, _)| i);
        if name_len == 0 {
            return None;
        }
        let name = &rest[..name_len];
        rest = rest[name_len..].strip_prefix('{')?;
        let mut args = serde_json::Map::new();
        if let Some(after) = rest.strip_prefix('}') {
            rest = after;
        } else {
            loop {
                let colon = rest.find(':')?;
                let key = &rest[..colon];
                if key.is_empty()
                    || !key
                        .chars()
                        .all(|c| c == '_' || c == '.' || c.is_ascii_alphanumeric())
                {
                    return None;
                }
                rest = &rest[colon + 1..];
                let value = if let Some(q) = rest.strip_prefix(QUOTE) {
                    let end = q.find(QUOTE)?;
                    rest = &q[end + QUOTE.len()..];
                    Value::String(q[..end].to_string())
                } else {
                    let end = rest.find([',', '}'])?;
                    let raw = rest[..end].trim();
                    rest = &rest[end..];
                    serde_json::from_str::<Value>(raw)
                        .ok()
                        .filter(|v| v.is_number() || v.is_boolean() || v.is_null())
                        .unwrap_or_else(|| Value::String(raw.to_string()))
                };
                args.insert(key.to_string(), value);
                if let Some(after) = rest.strip_prefix(',') {
                    rest = after;
                } else {
                    rest = rest.strip_prefix('}')?;
                    break;
                }
            }
        }
        calls.push(ToolCall {
            id: format!("call_{}", calls.len()),
            name: name.to_string(),
            arguments: Value::Object(args),
        });
        rest = rest.trim_start();
    }
    if calls.is_empty() { None } else { Some(calls) }
}
