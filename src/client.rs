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
    if text.is_empty() {
        return Err(KiError::BadResponse("leere Antwort".into()));
    }
    Ok(Reply::Text(text.to_string()))
}
