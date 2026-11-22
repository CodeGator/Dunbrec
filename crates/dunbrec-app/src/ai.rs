//! Talking to a language model: Ollama (running on this computer or
//! another one) or Claude (Anthropic's service).
//!
//! Everything slow happens on a worker thread; the interface only sees
//! [`Event`]s arriving on a channel. Building requests and reading the
//! replies are plain functions, so they are tested without a network.

use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
/// Tokens of text an Ollama model may read at once. Ollama's own default is
/// far too small for a circuit and its firmware.
pub const DEFAULT_OLLAMA_CONTEXT: u32 = 16_384;
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-sonnet-5-5";
const CLAUDE_URL: &str = "https://api.anthropic.com";
const CLAUDE_VERSION: &str = "2023-06-01";
/// Most tokens Claude may write in one answer.
const CLAUDE_MAX_TOKENS: u32 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Provider {
    #[default]
    Ollama,
    Claude,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Ollama => "Ollama",
            Provider::Claude => "Claude",
        }
    }
}

/// What the user chose in Settings (the Claude API key is not here: it is
/// kept in the Windows Credential Manager, see [`key`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    pub provider: Provider,
    pub ollama_url: String,
    pub ollama_model: String,
    pub ollama_context: u32,
    pub claude_model: String,
}

impl Default for AiSettings {
    fn default() -> Self {
        AiSettings {
            provider: Provider::Ollama,
            ollama_url: DEFAULT_OLLAMA_URL.into(),
            ollama_model: String::new(),
            ollama_context: DEFAULT_OLLAMA_CONTEXT,
            claude_model: DEFAULT_CLAUDE_MODEL.into(),
        }
    }
}

impl AiSettings {
    /// The model of the chosen provider.
    pub fn model(&self) -> &str {
        match self.provider {
            Provider::Ollama => &self.ollama_model,
            Provider::Claude => &self.claude_model,
        }
    }

    /// What is missing before a chat can start, if anything.
    pub fn problem(&self) -> Option<&'static str> {
        match self.provider {
            Provider::Ollama if self.ollama_url.trim().is_empty() => {
                Some("Enter the address of your Ollama server in Settings.")
            }
            Provider::Ollama if self.ollama_model.trim().is_empty() => {
                Some("Choose an Ollama model in Settings.")
            }
            Provider::Claude if self.claude_model.trim().is_empty() => {
                Some("Choose a Claude model in Settings.")
            }
            Provider::Claude if !key::exists() => Some("Enter your Claude API key in Settings."),
            _ => None,
        }
    }
}

/// The Claude API key, in the Windows Credential Manager.
pub mod key {
    const SERVICE: &str = "Dunbrec";
    const USER: &str = "claude-api-key";

    fn entry() -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, USER).map_err(|e| e.to_string())
    }

    pub fn get() -> Option<String> {
        entry().ok()?.get_password().ok().filter(|k| !k.is_empty())
    }

    pub fn exists() -> bool {
        get().is_some()
    }

    pub fn set(key: &str) -> Result<(), String> {
        entry()?.set_password(key.trim()).map_err(|e| e.to_string())
    }

    pub fn remove() -> Result<(), String> {
        match entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub text: String,
}

/// What a worker thread reports.
#[derive(Debug)]
pub enum Event {
    /// More of the answer.
    Text(String),
    /// The answer is complete.
    Done,
    Failed(String),
}

/// Everything one request needs, copied so the worker owns it.
#[derive(Debug, Clone)]
pub struct Request {
    pub settings: AiSettings,
    pub system: String,
    pub messages: Vec<ChatMessage>,
}

/// A running request: read its events, or cancel it.
pub struct Chat {
    pub events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
}

impl Chat {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Start `request` on a worker thread. `repaint` is called whenever
/// something arrives, so the interface redraws.
pub fn start(request: Request, repaint: impl Fn() + Send + 'static) -> Chat {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    std::thread::spawn(move || {
        let result = run(&request, &tx, &flag, &repaint);
        let _ = tx.send(match result {
            Ok(()) => Event::Done,
            Err(e) => Event::Failed(e),
        });
        repaint();
    });
    Chat { events: rx, cancel }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        // A local model can take a while to load before the first word.
        .timeout_recv_response(Some(Duration::from_secs(300)))
        .build()
        .into()
}

fn run(
    request: &Request,
    tx: &Sender<Event>,
    cancel: &AtomicBool,
    repaint: &dyn Fn(),
) -> Result<(), String> {
    let agent = agent();
    let settings = &request.settings;
    let (url, headers, body) = match settings.provider {
        Provider::Ollama => (
            format!(
                "{}/api/chat",
                settings.ollama_url.trim().trim_end_matches('/')
            ),
            Vec::new(),
            ollama_body(request),
        ),
        Provider::Claude => {
            let key = key::get().ok_or("No Claude API key is saved.")?;
            (
                format!("{CLAUDE_URL}/v1/messages"),
                vec![
                    ("x-api-key", key),
                    ("anthropic-version", CLAUDE_VERSION.to_string()),
                ],
                claude_body(request),
            )
        }
    };
    let mut post = agent.post(&url).header("content-type", "application/json");
    for (name, value) in &headers {
        post = post.header(*name, value);
    }
    let mut response = post
        .send(body.to_string())
        .map_err(|e| format!("Could not reach {}: {e}", settings.provider.name()))?;
    let status = response.status().as_u16();
    if status != 200 {
        let text = response.body_mut().read_to_string().unwrap_or_default();
        return Err(http_error(status, &text));
    }
    let reader = BufReader::new(response.into_body().into_reader());
    for line in reader.lines() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let line = line.map_err(|e| format!("The connection was interrupted: {e}"))?;
        let delta = match settings.provider {
            Provider::Ollama => parse_ollama_line(&line),
            Provider::Claude => parse_claude_line(&line),
        };
        match delta {
            Delta::Text(t) => {
                if tx.send(Event::Text(t)).is_err() {
                    return Ok(());
                }
                repaint();
            }
            Delta::Finished => return Ok(()),
            Delta::Error(e) => return Err(e),
            Delta::Nothing => {}
        }
    }
    Ok(())
}

// ---- requests ----

fn ollama_body(request: &Request) -> Value {
    let mut messages = vec![json!({"role": "system", "content": request.system})];
    messages.extend(
        request
            .messages
            .iter()
            .map(|m| json!({"role": m.role.name(), "content": m.text})),
    );
    json!({
        "model": request.settings.ollama_model,
        "messages": messages,
        "stream": true,
        "options": {"num_ctx": request.settings.ollama_context},
    })
}

fn claude_body(request: &Request) -> Value {
    let messages: Vec<Value> = request
        .messages
        .iter()
        .map(|m| json!({"role": m.role.name(), "content": m.text}))
        .collect();
    json!({
        "model": request.settings.claude_model,
        "max_tokens": CLAUDE_MAX_TOKENS,
        "system": request.system,
        "messages": messages,
        "stream": true,
    })
}

// ---- replies ----

#[derive(Debug, PartialEq, Eq)]
enum Delta {
    Text(String),
    Finished,
    Error(String),
    Nothing,
}

/// One line of Ollama's reply: a JSON object per line.
fn parse_ollama_line(line: &str) -> Delta {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Delta::Nothing;
    };
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        return Delta::Error(e.to_string());
    }
    if v.get("done").and_then(Value::as_bool) == Some(true) {
        return Delta::Finished;
    }
    match v.pointer("/message/content").and_then(Value::as_str) {
        Some(t) if !t.is_empty() => Delta::Text(t.to_string()),
        _ => Delta::Nothing,
    }
}

/// One line of Claude's event stream; only `data:` lines matter.
fn parse_claude_line(line: &str) -> Delta {
    let Some(data) = line.strip_prefix("data:") else {
        return Delta::Nothing;
    };
    let Ok(v) = serde_json::from_str::<Value>(data.trim()) else {
        return Delta::Nothing;
    };
    match v.get("type").and_then(Value::as_str) {
        Some("content_block_delta") => match v.pointer("/delta/text").and_then(Value::as_str) {
            Some(t) if !t.is_empty() => Delta::Text(t.to_string()),
            _ => Delta::Nothing,
        },
        Some("message_stop") => Delta::Finished,
        Some("error") => Delta::Error(
            v.pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("Claude reported an error.")
                .to_string(),
        ),
        _ => Delta::Nothing,
    }
}

/// A readable message for a failed HTTP request.
fn http_error(status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body).ok().and_then(|v| {
        v.pointer("/error/message")
            .or_else(|| v.get("error"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let hint = match status {
        401 | 403 => " Check the API key in Settings.",
        404 => " Check the model name in Settings.",
        429 => " Too many requests; wait a moment and try again.",
        _ => "",
    };
    match detail {
        Some(d) => format!("The server answered {status}: {d}{hint}"),
        None => format!("The server answered {status}.{hint}"),
    }
}

// ---- model lists (also used by Settings → Test) ----

/// Names of the models an Ollama server has.
pub fn ollama_models(url: &str) -> Result<Vec<String>, String> {
    let url = format!("{}/api/tags", url.trim().trim_end_matches('/'));
    let text = get(&url, &[])?;
    parse_ollama_models(&text)
}

/// Names of the models Claude offers this key.
pub fn claude_models() -> Result<Vec<String>, String> {
    let key = key::get().ok_or("No Claude API key is saved.")?;
    let text = get(
        &format!("{CLAUDE_URL}/v1/models?limit=100"),
        &[("x-api-key", &key), ("anthropic-version", CLAUDE_VERSION)],
    )?;
    parse_claude_models(&text)
}

fn get(url: &str, headers: &[(&str, &str)]) -> Result<String, String> {
    let mut request = agent().get(url);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut response = request
        .call()
        .map_err(|e| format!("Could not connect: {e}"))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    if status == 200 {
        Ok(text)
    } else {
        Err(http_error(status, &text))
    }
}

fn parse_ollama_models(text: &str) -> Result<Vec<String>, String> {
    let v: Value = serde_json::from_str(text).map_err(|_| "Unexpected reply.".to_string())?;
    let mut names: Vec<String> = v
        .get("models")
        .and_then(Value::as_array)
        .ok_or("Unexpected reply: is this an Ollama server?")?
        .iter()
        .filter_map(|m| m.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    names.sort();
    Ok(names)
}

fn parse_claude_models(text: &str) -> Result<Vec<String>, String> {
    let v: Value = serde_json::from_str(text).map_err(|_| "Unexpected reply.".to_string())?;
    Ok(v.get("data")
        .and_then(Value::as_array)
        .ok_or("Unexpected reply.")?
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request {
        Request {
            settings: AiSettings {
                provider: Provider::Ollama,
                ollama_model: "llama3".into(),
                ..AiSettings::default()
            },
            system: "be brief".into(),
            messages: vec![
                ChatMessage {
                    role: Role::User,
                    text: "hi".into(),
                },
                ChatMessage {
                    role: Role::Assistant,
                    text: "hello".into(),
                },
            ],
        }
    }

    #[test]
    fn ollama_request_puts_the_system_text_first() {
        let b = ollama_body(&request());
        assert_eq!(b["model"], "llama3");
        assert_eq!(b["stream"], true);
        assert_eq!(b["options"]["num_ctx"], DEFAULT_OLLAMA_CONTEXT);
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][0]["content"], "be brief");
        assert_eq!(b["messages"][2]["role"], "assistant");
    }

    #[test]
    fn claude_request_keeps_the_system_text_apart() {
        let b = claude_body(&request());
        assert_eq!(b["system"], "be brief");
        assert_eq!(b["messages"].as_array().unwrap().len(), 2);
        assert_eq!(b["messages"][0]["role"], "user");
        assert!(b["max_tokens"].as_u64().unwrap() > 0);
    }

    #[test]
    fn ollama_lines_are_read() {
        assert_eq!(
            parse_ollama_line(r#"{"message":{"role":"assistant","content":"Hi"},"done":false}"#),
            Delta::Text("Hi".into())
        );
        assert_eq!(
            parse_ollama_line(r#"{"message":{"content":""},"done":true}"#),
            Delta::Finished
        );
        assert_eq!(
            parse_ollama_line(r#"{"error":"model not found"}"#),
            Delta::Error("model not found".into())
        );
        assert_eq!(parse_ollama_line("garbage"), Delta::Nothing);
    }

    #[test]
    fn claude_lines_are_read() {
        assert_eq!(parse_claude_line("event: ping"), Delta::Nothing);
        assert_eq!(
            parse_claude_line(
                r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#
            ),
            Delta::Text("Hel".into())
        );
        assert_eq!(
            parse_claude_line(r#"data: {"type":"message_stop"}"#),
            Delta::Finished
        );
        assert_eq!(
            parse_claude_line(
                r#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
            ),
            Delta::Error("Overloaded".into())
        );
    }

    #[test]
    fn http_errors_are_explained() {
        let m = http_error(401, r#"{"error":{"message":"invalid x-api-key"}}"#);
        assert!(m.contains("invalid x-api-key") && m.contains("API key"));
        assert!(http_error(404, r#"{"error":"model 'x' not found"}"#).contains("not found"));
        assert!(http_error(500, "<html>").contains("500"));
    }

    #[test]
    fn model_lists_are_read() {
        let o = parse_ollama_models(r#"{"models":[{"name":"b:7b"},{"name":"a:1b"}]}"#).unwrap();
        assert_eq!(o, ["a:1b", "b:7b"]);
        assert!(parse_ollama_models(r#"{"x":1}"#).is_err());
        let c = parse_claude_models(r#"{"data":[{"id":"claude-sonnet-5-5"}]}"#).unwrap();
        assert_eq!(c, ["claude-sonnet-5-5"]);
    }

    #[test]
    fn old_settings_without_ai_fields_still_load() {
        let s: AiSettings = serde_json::from_str(r#"{"provider":"Claude"}"#).unwrap();
        assert_eq!(s.provider, Provider::Claude);
        assert_eq!(s.ollama_url, DEFAULT_OLLAMA_URL);
    }
}
