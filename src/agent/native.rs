//! the built-in agent: any openai-compatible chat api (openai, openrouter,
//! groq, deepseek, ollama, lm studio, ...) driven from inside ligase. it
//! speaks acp to the panel like an outside agent does, so the conversation,
//! tool steps, permissions, file reads and writes and stop all go the same
//! tested way. its tools read and write through ligase (open files take the
//! edits, undo works), list and search the workspace, and run commands only
//! after the person allows it. when the talk nears the model's context, the
//! older part is summarized: auto compact.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::acp::{Fail, Incoming};

/// most model calls in one turn.
const MAX_STEPS: usize = 40;
/// longest server line taken while streaming.
const MAX_LINE: u64 = 4 * 1024 * 1024;
/// most text a tool hands back to the model.
const MAX_TOOL_TEXT: usize = 48 * 1024;
/// most entries a listing gives.
const MAX_LIST: usize = 400;
/// most search hits given.
const MAX_HITS: usize = 120;
/// a command's longest run.
const COMMAND_LIMIT: Duration = Duration::from_secs(120);

/// where and what to ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// up to and without `/chat/completions`, like `https://api.openai.com/v1`.
    pub base: String,
    pub model: String,
    pub key: Option<String>,
    /// the model's context, in tokens; compaction starts at three quarters.
    pub context: usize,
}

/// ready-made servers for the setup card: name, base url, a model to start
/// with, whether a key is needed.
pub const PRESETS: [(&str, &str, &str, bool); 7] = [
    ("OpenAI", "https://api.openai.com/v1", "gpt-4.1", true),
    // gemini without its node cli: google's openai-compatible endpoint
    (
        "Gemini",
        "https://generativelanguage.googleapis.com/v1beta/openai",
        "gemini-2.5-pro",
        true,
    ),
    (
        "OpenRouter",
        "https://openrouter.ai/api/v1",
        "anthropic/claude-sonnet-4",
        true,
    ),
    (
        "Groq",
        "https://api.groq.com/openai/v1",
        "llama-3.3-70b-versatile",
        true,
    ),
    (
        "DeepSeek",
        "https://api.deepseek.com/v1",
        "deepseek-chat",
        true,
    ),
    (
        "Ollama",
        "http://localhost:11434/v1",
        "qwen2.5-coder",
        false,
    ),
    (
        "LM Studio",
        "http://localhost:1234/v1",
        "local-model",
        false,
    ),
];

/// a server on this machine needs no key.
pub fn is_local(base: &str) -> bool {
    let rest = base
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    rest.starts_with("localhost") || rest.starts_with("127.0.0.1") || rest.starts_with("[::1]")
}

/// longest key kept.
const MAX_KEY: usize = 1024;

/// the keys file beside the settings: `"profile" = "key"` lines, one per
/// model profile. never in the settings file, which people share.
pub fn keys_path() -> Option<PathBuf> {
    aurea::config_dir("ligase").map(|d| d.join("agent-keys.toml"))
}

/// the keys file read; empty when missing or not toml. worker only.
fn read_keys() -> toml::Table {
    keys_path().map(|p| keys_in(&p)).unwrap_or_default()
}

fn keys_in(path: &Path) -> toml::Table {
    std::fs::read_to_string(path)
        .ok()
        .filter(|t| t.len() < 256 * 1024)
        .and_then(|t| t.parse::<toml::Table>().ok())
        .unwrap_or_default()
}

/// `profile`'s key: `LIGASE_AGENT_KEY` first, then the keys file, then
/// `OPENAI_API_KEY`. reads the disk: worker only.
pub fn read_key(profile: &str) -> Option<String> {
    let env = |name| std::env::var(name).ok().filter(|k| !k.trim().is_empty());
    env("LIGASE_AGENT_KEY")
        .or_else(|| {
            read_keys()
                .get(profile)
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|k| !k.is_empty() && k.len() < MAX_KEY)
                .map(str::to_owned)
        })
        .or_else(|| env("OPENAI_API_KEY"))
}

/// the profiles with a key in the file, to say so without showing it.
/// worker only.
pub fn keyed() -> Vec<String> {
    read_keys().keys().cloned().collect()
}

/// puts `profile`'s key in the file, or takes it out; the others stay.
/// readable only by its owner where the system allows saying so. worker
/// only.
pub fn set_key(profile: &str, key: Option<&str>) -> Result<(), String> {
    set_key_in(&keys_path().ok_or("no config folder")?, profile, key)
}

fn set_key_in(path: &Path, profile: &str, key: Option<&str>) -> Result<(), String> {
    let path = path.to_path_buf();
    // a file there that can't be read stays as it is: writing one profile
    // over it would lose every other key
    let mut keys = match std::fs::read_to_string(&path) {
        Ok(text) if text.len() >= 256 * 1024 => return Err("the keys file is too big".to_owned()),
        Ok(text) => text
            .parse::<toml::Table>()
            .map_err(|e| format!("the keys file is not valid toml, fix it first: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(e.to_string()),
    };
    match key.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) if k.len() < MAX_KEY => {
            keys.insert(profile.to_owned(), toml::Value::String(k.to_owned()));
        }
        Some(_) => return Err("the key is too long".to_owned()),
        None => {
            keys.remove(profile);
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = toml::to_string(&keys).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create(true).truncate(true);
    // owner only from the start, never readable by others for a moment
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
    }
    {
        use std::io::Write;
        let mut file = open.open(&tmp).map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// what the in-process agent shares between its threads.
struct Link {
    out: Sender<Incoming>,
    wake: Box<dyn Fn() + Send + Sync>,
    /// our requests to ligase, waiting for their answers.
    waiting: Mutex<HashMap<String, Sender<Result<Value, String>>>>,
    next: AtomicU64,
    cancel: AtomicBool,
    /// commands allowed for the rest of the session.
    commands_ok: AtomicBool,
    /// the mode the person picked: ask, auto, full or plan.
    mode: Mutex<&'static str>,
}

/// the built-in agent's modes: id, name, what it lets through.
const MODES: [(&str, &str, &str); 4] = [
    ("ask", "Ask first", "Asks before each edit or command"),
    (
        "auto",
        "Auto edits",
        "Edits files on its own, asks before commands",
    ),
    (
        "full",
        "Full auto",
        "Edits and runs anything without asking",
    ),
    ("plan", "Plan only", "Reads and plans, changes nothing"),
];

fn modes_json(current: &str) -> Value {
    json!({
        "currentModeId": current,
        "availableModes": MODES.iter().map(|(id, name, about)| json!({"id": id, "name": name, "description": about})).collect::<Vec<_>>(),
    })
}

impl Link {
    fn send(&self, message: Incoming) {
        let _ = self.out.send(message);
        (self.wake)();
    }

    fn update(&self, update: Value) {
        self.send(Incoming::Note(
            "session/update".to_owned(),
            json!({"sessionId": SESSION, "update": update}),
        ));
    }

    /// asks ligase and waits for the answer, or for the pipe to close.
    fn ask(&self, method: &str, params: Value, wait: Duration) -> Result<Value, String> {
        let id = format!("n{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = channel();
        if let Ok(mut w) = self.waiting.lock() {
            w.insert(id.clone(), tx);
        }
        self.send(Incoming::Request(
            Value::String(id.clone()),
            method.to_owned(),
            params,
        ));
        let answer = rx
            .recv_timeout(wait)
            .map_err(|_| "no answer from the editor".to_owned());
        if let Ok(mut w) = self.waiting.lock() {
            w.remove(&id);
        }
        answer?
    }
}

const SESSION: &str = "ligase";

/// runs the agent on the lines ligase sends, until the pipe closes.
pub fn serve(
    config: Config,
    cwd: PathBuf,
    lines: Receiver<String>,
    out: Sender<Incoming>,
    wake: Box<dyn Fn() + Send + Sync>,
) {
    let link = Arc::new(Link {
        out,
        wake,
        waiting: Mutex::default(),
        next: AtomicU64::new(1),
        cancel: AtomicBool::new(false),
        commands_ok: AtomicBool::new(false),
        mode: Mutex::new("auto"),
    });
    let history: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![json!({
        "role": "system",
        "content": system_prompt(&cwd),
    })]));
    for line in lines {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = message["method"].as_str();
        let id = message.get("id").cloned();
        match (method, id) {
            (Some(method), Some(id)) => {
                let Some(id) = id.as_u64() else { continue };
                let reply = |result: Result<Value, Fail>| link.send(Incoming::Reply(id, result));
                match method {
                    "initialize" => reply(Ok(json!({
                        "protocolVersion": 1,
                        "agentCapabilities": {"loadSession": false},
                        "authMethods": [{"id": "setup", "name": "Set up the model"}],
                    }))),
                    "session/new" | "authenticate" => {
                        let missing = config.model.trim().is_empty()
                            || (config.key.is_none() && !is_local(&config.base));
                        if missing {
                            reply(Err(Fail {
                                code: -32000,
                                message: "Authentication required: set the model and its key"
                                    .to_owned(),
                            }));
                        } else {
                            let mode = link.mode.lock().map_or("auto", |m| *m);
                            reply(Ok(json!({"sessionId": SESSION, "modes": modes_json(mode)})));
                        }
                    }
                    "session/set_mode" => {
                        let asked = message["params"]["modeId"].as_str().unwrap_or("");
                        match MODES.iter().find(|(id, ..)| *id == asked) {
                            Some((id, ..)) => {
                                if let Ok(mut m) = link.mode.lock() {
                                    *m = id;
                                }
                                reply(Ok(json!({})));
                                link.update(json!({"sessionUpdate": "current_mode_update", "currentModeId": id}));
                            }
                            None => reply(Err(Fail {
                                code: -32602,
                                message: format!("no mode {asked}"),
                            })),
                        }
                    }
                    "session/prompt" => {
                        link.cancel.store(false, Ordering::Relaxed);
                        let (link, history, config, cwd) = (
                            Arc::clone(&link),
                            Arc::clone(&history),
                            config.clone(),
                            cwd.clone(),
                        );
                        let prompt = message["params"]["prompt"].clone();
                        let started = std::thread::Builder::new()
                            .name("ligase-agent-turn".into())
                            .spawn(move || {
                                let stop = turn(&link, &history, &config, &cwd, &prompt);
                                let result = match stop {
                                    Ok(stop) => Ok(json!({"stopReason": stop})),
                                    Err(message) => Err(Fail {
                                        code: -32603,
                                        message,
                                    }),
                                };
                                link.send(Incoming::Reply(id, result));
                            });
                        if started.is_err() {
                            reply(Err(Fail {
                                code: -32603,
                                message: "could not start the turn".to_owned(),
                            }));
                        }
                    }
                    _ => reply(Err(Fail {
                        code: -32601,
                        message: format!("{method} is not supported"),
                    })),
                }
            }
            (Some("session/cancel"), None) => link.cancel.store(true, Ordering::Relaxed),
            (None, Some(Value::String(id))) => {
                let result = match message.get("error") {
                    Some(e) => Err(e["message"].as_str().unwrap_or("failed").to_owned()),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let waiter = link.waiting.lock().ok().and_then(|mut w| w.remove(&id));
                if let Some(tx) = waiter {
                    let _ = tx.send(result);
                }
            }
            _ => {}
        }
    }
    // the pipe closed: a turn still going stops at its next step
    link.cancel.store(true, Ordering::Relaxed);
}

fn system_prompt(cwd: &Path) -> String {
    format!(
        "You are a coding agent inside the Ligase editor, working in the folder {}. \
         Use the tools: read before you change, change with edit_file where you can, \
         run commands only when they help. Paths are relative to the folder. \
         Keep answers short and plain; say what you changed.",
        cwd.display()
    )
}

/// one prompt turn: the model, its tools, again, until it answers without a
/// tool or is stopped. gives acp's stop reason.
fn turn(
    link: &Link,
    history: &Mutex<Vec<Value>>,
    config: &Config,
    cwd: &Path,
    prompt: &Value,
) -> Result<&'static str, String> {
    let user = user_message(link, prompt);
    lock(history)?.push(json!({"role": "user", "content": user}));
    for _ in 0..MAX_STEPS {
        if link.cancel.load(Ordering::Relaxed) {
            return Ok("cancelled");
        }
        compact(link, history, config)?;
        let messages = lock(history)?.clone();
        let (text, calls) = stream(link, config, &messages)?;
        if link.cancel.load(Ordering::Relaxed) {
            if !text.is_empty() {
                lock(history)?.push(json!({"role": "assistant", "content": text}));
            }
            return Ok("cancelled");
        }
        let mut assistant = json!({"role": "assistant", "content": text});
        if !calls.is_empty() {
            assistant["tool_calls"] = Value::Array(
                calls
                    .iter()
                    .map(|c| json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.arguments}}))
                    .collect(),
            );
        }
        lock(history)?.push(assistant);
        if calls.is_empty() {
            return Ok("end_turn");
        }
        for call in calls {
            let result = if link.cancel.load(Ordering::Relaxed) {
                "stopped before running".to_owned()
            } else {
                run_tool(link, cwd, &call)
            };
            lock(history)?.push(json!({"role": "tool", "tool_call_id": call.id, "content": cut(&result, MAX_TOOL_TEXT)}));
        }
    }
    Ok("max_turn_requests")
}

fn lock<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, String> {
    m.lock().map_err(|_| "the conversation broke".to_owned())
}

/// the person's message: their text, then each file they put before it,
/// read through ligase so unsaved text counts.
fn user_message(link: &Link, prompt: &Value) -> String {
    let mut out = String::new();
    for block in prompt.as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => {
                out.push_str(block["text"].as_str().unwrap_or(""));
                out.push('\n');
            }
            Some("resource_link") => {
                let uri = block["uri"].as_str().unwrap_or("");
                let Some((path, lines)) = file_uri(uri) else {
                    continue;
                };
                let params = match lines {
                    Some((a, b)) => {
                        json!({"sessionId": SESSION, "path": path, "line": a, "limit": b + 1 - a})
                    }
                    None => json!({"sessionId": SESSION, "path": path}),
                };
                let text = link
                    .ask("fs/read_text_file", params, Duration::from_secs(30))
                    .ok()
                    .and_then(|v| v["content"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| "(could not read)".to_owned());
                let span = lines.map_or(String::new(), |(a, b)| format!(" lines=\"{a}-{b}\""));
                out.push_str(&format!(
                    "\n<file path=\"{}\"{span}>\n{}\n</file>\n",
                    path.display(),
                    cut(&text, MAX_TOOL_TEXT)
                ));
            }
            _ => {}
        }
    }
    out
}

/// `file:///path#L3-9` as the path and lines.
fn file_uri(uri: &str) -> Option<(PathBuf, Option<(usize, usize)>)> {
    let rest = uri.strip_prefix("file://")?;
    let (path, lines) = match rest.split_once("#L") {
        Some((p, l)) => {
            let (a, b) = l.split_once('-').unwrap_or((l, l));
            (p, Some((a.parse().ok()?, b.parse().ok()?)))
        }
        None => (rest, None),
    };
    // `/C:/x` on windows is `C:/x`
    let path = if path.len() > 2 && path.as_bytes()[2] == b':' {
        &path[1..]
    } else {
        path
    };
    Some((PathBuf::from(path), lines))
}

fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… (cut, {} more bytes)", &text[..end], text.len() - end)
}

/// a tool call the model made, put together from the stream.
#[derive(Clone, Debug, Default, PartialEq)]
struct Call {
    id: String,
    name: String,
    arguments: String,
}

fn tools() -> Value {
    let tool = |name: &str, about: &str, props: Value, required: &[&str]| {
        json!({"type": "function", "function": {
            "name": name,
            "description": about,
            "parameters": {"type": "object", "properties": props, "required": required},
        }})
    };
    json!([
        tool(
            "read_file",
            "Read a text file. Lines count from 1.",
            json!({"path": {"type": "string"}, "line": {"type": "integer"}, "limit": {"type": "integer"}}),
            &["path"]
        ),
        tool(
            "edit_file",
            "Replace one exact piece of text in a file with another. old_text must appear exactly once.",
            json!({"path": {"type": "string"}, "old_text": {"type": "string"}, "new_text": {"type": "string"}}),
            &["path", "old_text", "new_text"]
        ),
        tool(
            "write_file",
            "Write a whole file, making it when missing.",
            json!({"path": {"type": "string"}, "content": {"type": "string"}}),
            &["path", "content"]
        ),
        tool(
            "list_files",
            "List the files under a folder, leaving out what .gitignore ignores.",
            json!({"path": {"type": "string"}}),
            &[]
        ),
        tool(
            "search",
            "Find a text in the files under a folder; gives path:line: text.",
            json!({"query": {"type": "string"}, "path": {"type": "string"}}),
            &["query"]
        ),
        tool(
            "run_command",
            "Run a shell command in the workspace and get its output. The person is asked first.",
            json!({"command": {"type": "string"}}),
            &["command"]
        ),
    ])
}

/// sends the conversation and streams the answer: text goes out as it
/// comes, tool calls are gathered. gives the text and the calls.
fn stream(link: &Link, config: &Config, messages: &[Value]) -> Result<(String, Vec<Call>), String> {
    let body = json!({
        "model": config.model,
        "messages": messages,
        "tools": tools(),
        "stream": true,
    });
    let response = post(config, &body)?;
    let mut reader = BufReader::new(response.into_reader().take(256 * 1024 * 1024));
    let mut text = String::new();
    let mut calls: Vec<Call> = Vec::new();
    let mut line = String::new();
    loop {
        if link.cancel.load(Ordering::Relaxed) {
            break;
        }
        line.clear();
        let read = reader
            .by_ref()
            .take(MAX_LINE)
            .read_line(&mut line)
            .map_err(|e| format!("reading the answer: {e}"))?;
        if read == 0 {
            break;
        }
        let Some(data) = line.trim_end().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(message) = chunk["error"]["message"].as_str() {
            return Err(message.to_owned());
        }
        let delta = &chunk["choices"][0]["delta"];
        for key in ["reasoning_content", "reasoning"] {
            if let Some(t) = delta[key].as_str().filter(|t| !t.is_empty()) {
                link.update(json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": t}}));
            }
        }
        if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            text.push_str(t);
            link.update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": t}}));
        }
        for part in delta["tool_calls"].as_array().into_iter().flatten() {
            let n = part["index"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(calls.len());
            if n > 64 {
                continue;
            }
            while calls.len() <= n {
                calls.push(Call::default());
            }
            let call = &mut calls[n];
            if let Some(id) = part["id"].as_str() {
                call.id = id.to_owned();
            }
            if let Some(name) = part["function"]["name"].as_str() {
                call.name.push_str(name);
            }
            if let Some(args) = part["function"]["arguments"].as_str()
                && call.arguments.len() < MAX_TOOL_TEXT * 4
            {
                call.arguments.push_str(args);
            }
        }
    }
    // some servers leave the id out; the history still needs one
    for (i, call) in calls.iter_mut().enumerate() {
        if call.id.is_empty() {
            call.id = format!("call_{i}");
        }
    }
    calls.retain(|c| !c.name.is_empty());
    Ok((text, calls))
}

/// posts `body` to the chat endpoint; an http error gives the server's own
/// message.
fn post(config: &Config, body: &Value) -> Result<ureq::Response, String> {
    let agent = https()?
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(180))
        .build();
    let url = format!("{}/chat/completions", config.base.trim_end_matches('/'));
    let mut request = agent.post(&url).set("Content-Type", "application/json");
    if let Some(key) = &config.key {
        request = request.set("Authorization", &format!("Bearer {key}"));
    }
    match request.send_string(&body.to_string()) {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(code, response)) => {
            let text = response.into_string().unwrap_or_default();
            let said = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
                .unwrap_or_else(|| text.chars().take(300).collect());
            Err(format!("{url}: {code} {said}"))
        }
        Err(e) => Err(format!("{url}: {e}")),
    }
}

/// an agent builder that speaks https: schannel on windows, the security
/// framework on macos, both with the system's roots; rustls elsewhere.
fn https() -> Result<ureq::AgentBuilder, String> {
    let builder = ureq::AgentBuilder::new();
    #[cfg(any(windows, target_os = "macos"))]
    let builder = builder.tls_connector(std::sync::Arc::new(
        ureq::native_tls::TlsConnector::new().map_err(|e| format!("tls: {e}"))?,
    ));
    Ok(builder)
}

/// a rough token count: four bytes a token.
fn tokens(messages: &[Value]) -> usize {
    messages.iter().map(|m| m.to_string().len() / 4).sum()
}

/// when the conversation passes three quarters of the context, everything
/// but the system prompt and the last few messages is summarized into one
/// note. the cut lands before a user message, so a tool call and its
/// answer stay together.
fn compact(link: &Link, history: &Mutex<Vec<Value>>, config: &Config) -> Result<(), String> {
    let messages = lock(history)?.clone();
    if tokens(&messages) < config.context * 3 / 4 || messages.len() < 8 {
        return Ok(());
    }
    let Some(cut) = (2..messages.len().saturating_sub(3))
        .rev()
        .find(|&i| messages[i]["role"] == "user")
    else {
        return Ok(());
    };
    link.update(
        json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text",
        "text": "The conversation is long; summarizing the earlier part to keep room."}}),
    );
    let old: Vec<Value> = messages[1..cut].to_vec();
    let body = json!({
        "model": config.model,
        "messages": [
            {"role": "system", "content": "Summarize this coding conversation so the work can go on from it: the goal, what was decided, files read and changed, commands run and their results, what is left. Plain and short."},
            {"role": "user", "content": serde_json::to_string(&old).unwrap_or_default()},
        ],
        "stream": false,
    });
    let summary: Value = post(config, &body)?
        .into_json()
        .map_err(|e| format!("reading the summary: {e}"))?;
    let summary = summary["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    if summary.is_empty() {
        return Ok(());
    }
    let mut h = lock(history)?;
    // the history may have moved on only by appends; keep what came after
    let at = cut.min(h.len());
    let tail: Vec<Value> = h.drain(at..).collect();
    h.truncate(1);
    h.push(json!({"role": "user", "content": format!("Summary of the conversation so far:\n{summary}")}));
    h.push(json!({"role": "assistant", "content": "Understood. Going on from there."}));
    h.extend(tail);
    Ok(())
}

/// runs one tool; what it gives back goes to the model. each run shows as
/// a tool step in the panel.
fn run_tool(link: &Link, cwd: &Path, call: &Call) -> String {
    let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
    let text = |key: &str| args[key].as_str().unwrap_or("").to_owned();
    let path = |key: &str| {
        let p = text(key);
        let p = if p.is_empty() { ".".to_owned() } else { p };
        let joined = cwd.join(&p);
        super::inside(cwd, &joined).or_else(|| (joined == cwd).then(|| cwd.to_path_buf()))
    };
    let (kind, title) = match call.name.as_str() {
        "read_file" => ("read", format!("Read {}", text("path"))),
        "edit_file" | "write_file" => ("edit", format!("Edit {}", text("path"))),
        "list_files" => (
            "search",
            format!(
                "List {}",
                if text("path").is_empty() {
                    ".".to_owned()
                } else {
                    text("path")
                }
            ),
        ),
        "search" => ("search", format!("Search for {}", text("query"))),
        "run_command" => ("execute", format!("Run {}", text("command"))),
        other => ("other", other.to_owned()),
    };
    let place = path("path");
    let locations: Vec<Value> = place.iter().map(|p| json!({"path": p})).collect();
    link.update(
        json!({"sessionUpdate": "tool_call", "toolCallId": call.id, "title": title,
        "kind": kind, "status": "in_progress", "locations": locations}),
    );
    let mode = link.mode.lock().map_or("auto", |m| *m);
    let changes = matches!(
        call.name.as_str(),
        "edit_file" | "write_file" | "run_command"
    );
    let edits = matches!(call.name.as_str(), "edit_file" | "write_file");
    // plan only reads; ask first asks before an edit too
    let refused = if mode == "plan" && changes {
        Some("plan mode: only reading is allowed. say what you would change instead".to_owned())
    } else if mode == "ask" && edits {
        permit(link, call, &title).err()
    } else {
        None
    };
    let (result, content) = match call.name.as_str() {
        _ if refused.is_some() => (Err(refused.unwrap_or_default()), Value::Null),
        "read_file" => read_tool(link, place.as_deref(), &args),
        "edit_file" => edit_tool(link, place.as_deref(), &text("old_text"), &text("new_text")),
        "write_file" => write_tool(link, place.as_deref(), &text("content")),
        "list_files" => (list_tool(cwd, place.as_deref()), Value::Null),
        "search" => (
            search_tool(cwd, place.as_deref(), &text("query")),
            Value::Null,
        ),
        "run_command" => (command_tool(link, cwd, call, &text("command")), Value::Null),
        other => (Err(format!("there is no tool called {other}")), Value::Null),
    };
    let (status, out) = match result {
        Ok(out) => ("completed", out),
        Err(why) => ("failed", format!("error: {why}")),
    };
    let mut update =
        json!({"sessionUpdate": "tool_call_update", "toolCallId": call.id, "status": status});
    if content.is_array() {
        update["content"] = content;
    } else if status == "failed" || call.name == "run_command" {
        update["content"] =
            json!([{"type": "content", "content": {"type": "text", "text": cut(&out, 4000)}}]);
    }
    link.update(update);
    out
}

type ToolResult = (Result<String, String>, Value);

fn read(link: &Link, path: &Path) -> Result<String, String> {
    link.ask(
        "fs/read_text_file",
        json!({"sessionId": SESSION, "path": path}),
        Duration::from_secs(30),
    )?["content"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "no text came back".to_owned())
}

fn write(link: &Link, path: &Path, text: &str) -> Result<(), String> {
    link.ask(
        "fs/write_text_file",
        json!({"sessionId": SESSION, "path": path, "content": text}),
        Duration::from_secs(60),
    )
    .map(drop)
}

fn diff(path: &Path, old: Option<&str>, new: &str) -> Value {
    json!([{"type": "diff", "path": path, "oldText": old, "newText": new}])
}

fn read_tool(link: &Link, path: Option<&Path>, args: &Value) -> ToolResult {
    let Some(path) = path else {
        return (
            Err("the path is outside the workspace".to_owned()),
            Value::Null,
        );
    };
    let line = args["line"].as_u64();
    let limit = args["limit"].as_u64();
    let mut params = json!({"sessionId": SESSION, "path": path});
    if let Some(l) = line {
        params["line"] = json!(l);
    }
    if let Some(l) = limit {
        params["limit"] = json!(l);
    }
    let got = link
        .ask("fs/read_text_file", params, Duration::from_secs(30))
        .and_then(|v| {
            v["content"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "no text came back".to_owned())
        });
    (got, Value::Null)
}

fn edit_tool(link: &Link, path: Option<&Path>, old: &str, new: &str) -> ToolResult {
    let Some(path) = path else {
        return (
            Err("the path is outside the workspace".to_owned()),
            Value::Null,
        );
    };
    if old.is_empty() {
        return (
            Err("old_text is empty; use write_file to make a file".to_owned()),
            Value::Null,
        );
    }
    let now = match read(link, path) {
        Ok(t) => t,
        Err(e) => return (Err(e), Value::Null),
    };
    let found = now.matches(old).count();
    if found != 1 {
        return (
            Err(format!(
                "old_text is found {found} times; it must be found once. read the file and give more of it"
            )),
            Value::Null,
        );
    }
    let changed = now.replacen(old, new, 1);
    match write(link, path, &changed) {
        Ok(()) => (
            Ok(format!("edited {}", path.display())),
            diff(path, Some(&now), &changed),
        ),
        Err(e) => (Err(e), Value::Null),
    }
}

fn write_tool(link: &Link, path: Option<&Path>, text: &str) -> ToolResult {
    let Some(path) = path else {
        return (
            Err("the path is outside the workspace".to_owned()),
            Value::Null,
        );
    };
    let before = read(link, path).ok();
    match write(link, path, text) {
        Ok(()) => (
            Ok(format!("wrote {}", path.display())),
            diff(path, before.as_deref(), text),
        ),
        Err(e) => (Err(e), Value::Null),
    }
}

fn list_tool(cwd: &Path, dir: Option<&Path>) -> Result<String, String> {
    let dir = dir.ok_or("the path is outside the workspace")?;
    let mut out = Vec::new();
    let never = AtomicBool::new(false);
    crate::search::files(dir, &never, &mut |p| {
        out.push(
            p.strip_prefix(cwd)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/"),
        );
        out.len() < MAX_LIST
    });
    if out.is_empty() {
        return Ok("(no files)".to_owned());
    }
    let more = if out.len() >= MAX_LIST {
        "\n… (more not listed)"
    } else {
        ""
    };
    Ok(out.join("\n") + more)
}

fn search_tool(cwd: &Path, dir: Option<&Path>, query: &str) -> Result<String, String> {
    let dir = dir.ok_or("the path is outside the workspace")?;
    if query.is_empty() {
        return Err("the query is empty".to_owned());
    }
    let hits = crate::search::grep(dir, query, MAX_HITS);
    if hits.is_empty() {
        return Ok("(no matches)".to_owned());
    }
    Ok(hits
        .iter()
        .map(|h| {
            let rel = h
                .path
                .strip_prefix(cwd)
                .unwrap_or(&h.path)
                .to_string_lossy()
                .replace('\\', "/");
            format!("{rel}:{}: {}", h.line + 1, h.preview)
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// asks the person once about `call`; an error when they say no.
fn permit(link: &Link, call: &Call, title: &str) -> Result<(), String> {
    let answer = link.ask(
        "session/request_permission",
        json!({
            "sessionId": SESSION,
            "toolCall": {"toolCallId": call.id, "title": cut(title, 200)},
            "options": [
                {"optionId": "once", "name": "Allow", "kind": "allow_once"},
                {"optionId": "no", "name": "Don't", "kind": "reject_once"},
            ],
        }),
        Duration::from_secs(60 * 60),
    )?;
    match answer["outcome"]["optionId"].as_str() {
        Some("once") => Ok(()),
        _ => Err("the person did not allow it".to_owned()),
    }
}

/// runs `command` after the person allows it, with a time limit and its
/// output capped.
fn command_tool(link: &Link, cwd: &Path, call: &Call, command: &str) -> Result<String, String> {
    if command.trim().is_empty() {
        return Err("the command is empty".to_owned());
    }
    let full = link.mode.lock().is_ok_and(|m| *m == "full");
    if !full && !link.commands_ok.load(Ordering::Relaxed) {
        let answer = link.ask(
            "session/request_permission",
            json!({
                "sessionId": SESSION,
                "toolCall": {"toolCallId": call.id, "title": format!("Run `{}`", cut(command, 200))},
                "options": [
                    {"optionId": "once", "name": "Allow", "kind": "allow_once"},
                    {"optionId": "always", "name": "Allow all commands", "kind": "allow_always"},
                    {"optionId": "no", "name": "Don't run", "kind": "reject_once"},
                ],
            }),
            Duration::from_secs(60 * 60),
        )?;
        let picked = answer["outcome"]["optionId"].as_str().unwrap_or("");
        match picked {
            "once" => {}
            "always" => link.commands_ok.store(true, Ordering::Relaxed),
            _ => return Err("the person did not allow it".to_owned()),
        }
    }
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/d", "/s", "/c", command]);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", command]);
        c
    };
    cmd.current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    // readers fill a shared buffer; a program left running in the
    // background can hold the pipes open past the command, so they are
    // never waited on for longer than a moment
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let buf = std::sync::Arc::new(Mutex::new(Vec::new()));
        let (done, finished) = std::sync::mpsc::channel::<()>();
        let fill = std::sync::Arc::clone(&buf);
        std::thread::spawn(move || {
            if let Some(mut p) = pipe {
                let mut chunk = [0u8; 8 * 1024];
                loop {
                    let n = match p.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    let Ok(mut b) = fill.lock() else { break };
                    let room = (1024 * 1024usize).saturating_sub(b.len());
                    b.extend_from_slice(&chunk[..n.min(room)]);
                }
            }
            let _ = done.send(());
        });
        (buf, finished)
    };
    let take = |(buf, finished): (std::sync::Arc<Mutex<Vec<u8>>>, Receiver<()>)| {
        let _ = finished.recv_timeout(Duration::from_secs(2));
        buf.lock().map(|b| b.clone()).unwrap_or_default()
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None)
                if started.elapsed() < COMMAND_LIMIT && !link.cancel.load(Ordering::Relaxed) =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let mut text = String::from_utf8_lossy(&take(out)).into_owned();
    let e = String::from_utf8_lossy(&take(err)).into_owned();
    if !e.trim().is_empty() {
        text.push_str("\n[stderr]\n");
        text.push_str(&e);
    }
    match status {
        Some(s) => Ok(format!(
            "exit {}\n{}",
            s.code().unwrap_or(-1),
            cut(&text, MAX_TOOL_TEXT)
        )),
        None => Ok(format!(
            "stopped after {} s\n{}",
            COMMAND_LIMIT.as_secs(),
            cut(&text, MAX_TOOL_TEXT)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_links_read_back() {
        assert_eq!(
            file_uri("file:///C:/w/a.rs#L3-9"),
            Some((PathBuf::from("C:/w/a.rs"), Some((3, 9))))
        );
        assert_eq!(
            file_uri("file:///w/a.rs"),
            Some((PathBuf::from("/w/a.rs"), None))
        );
        assert_eq!(file_uri("https://x"), None);
        assert_eq!(file_uri("file:///w/a.rs#Lx-9"), None);
    }

    #[test]
    fn a_broken_keys_file_is_never_written_over() {
        let dir = std::env::temp_dir().join(format!("ligase-badkeys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("agent-keys.toml");
        std::fs::write(
            &path,
            "work = \"sk-1\"
home = ",
        )
        .expect("write");
        assert!(set_key_in(&path, "new", Some("sk-2")).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "work = \"sk-1\"
home = "
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_are_kept_apart_by_profile() {
        let path = std::env::temp_dir().join(format!("ligase-keys-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        set_key_in(&path, "work", Some(" sk-1 ")).unwrap();
        set_key_in(&path, "my \"home\" box", Some("sk-2")).unwrap();
        let keys = keys_in(&path);
        assert_eq!(keys["work"].as_str(), Some("sk-1"), "trimmed");
        assert_eq!(keys["my \"home\" box"].as_str(), Some("sk-2"));
        set_key_in(&path, "work", None).unwrap();
        assert!(!keys_in(&path).contains_key("work"));
        assert!(set_key_in(&path, "x", Some(&"k".repeat(2000))).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn local_servers_need_no_key() {
        assert!(is_local("http://localhost:11434/v1"));
        assert!(is_local("http://127.0.0.1:1234/v1"));
        assert!(!is_local("https://api.openai.com/v1"));
    }

    #[test]
    fn cuts_land_on_char_edges() {
        let text = "가".repeat(10);
        let c = cut(&text, 4);
        assert!(c.starts_with("가") && c.contains("more bytes"));
        assert_eq!(cut("short", 10), "short");
    }

    #[test]
    fn a_session_without_a_model_asks_for_setup() {
        let (tx, lines) = channel();
        let (out, from) = channel();
        let config = Config {
            base: "https://api.openai.com/v1".into(),
            model: String::new(),
            key: None,
            context: 32_000,
        };
        let agent = std::thread::spawn(move || {
            serve(config, PathBuf::from("."), lines, out, Box::new(|| {}))
        });
        tx.send(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}).to_string(),
        )
        .unwrap();
        tx.send(
            json!({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {}}).to_string(),
        )
        .unwrap();
        drop(tx);
        agent.join().unwrap();
        let got: Vec<Incoming> = from.try_iter().collect();
        assert!(
            matches!(&got[0], Incoming::Reply(1, Ok(v)) if v["authMethods"][0]["id"] == "setup")
        );
        assert!(matches!(
            &got[1],
            Incoming::Reply(2, Err(Fail { code: -32000, .. }))
        ));
    }

    /// a stand-in openai server: the first call asks to read a.txt, the
    /// next answers. gives each request body it got.
    fn fake_server() -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!(
            "http://127.0.0.1:{}/v1",
            listener.local_addr().unwrap().port()
        );
        let (seen, bodies) = channel();
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().take(2).enumerate() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut len = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    if line.trim_end().is_empty() {
                        break;
                    }
                }
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                seen.send(String::from_utf8(body).unwrap()).unwrap();
                let events = if n == 0 {
                    vec![
                        json!({"choices": [{"delta": {"content": "Looking."}}]}),
                        json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "c1", "function": {"name": "read_file", "arguments": "{\"path\":"}}]}}]}),
                        json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "\"a.txt\"}"}}]}}]}),
                    ]
                } else {
                    vec![json!({"choices": [{"delta": {"content": "It says hello."}}]})]
                };
                let mut out = String::from(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
                );
                for e in events {
                    out.push_str(&format!("data: {e}\n\n"));
                }
                out.push_str("data: [DONE]\n\n");
                stream.write_all(out.as_bytes()).unwrap();
            }
        });
        (base, bodies)
    }

    #[test]
    fn a_turn_streams_runs_a_tool_and_answers() {
        let (base, bodies) = fake_server();
        let (tx, lines) = channel();
        let (out, from) = channel();
        let config = Config {
            base,
            model: "m".into(),
            key: None,
            context: 32_000,
        };
        let cwd = std::env::temp_dir().join(format!("ligase-native-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).unwrap();
        let root = cwd.clone();
        std::thread::spawn(move || serve(config, root, lines, out, Box::new(|| {})));
        let send = |v: Value| tx.send(v.to_string()).unwrap();
        send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}));
        send(json!({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {}}));
        send(
            json!({"jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": {"prompt": [{"type": "text", "text": "what is in a.txt?"}]}}),
        );
        let mut text = String::new();
        let mut tool_done = false;
        let stop = loop {
            match from
                .recv_timeout(Duration::from_secs(20))
                .expect("the turn ends")
            {
                Incoming::Request(id, method, params) => {
                    assert_eq!(method, "fs/read_text_file");
                    assert!(params["path"].as_str().unwrap().ends_with("a.txt"));
                    send(json!({"jsonrpc": "2.0", "id": id, "result": {"content": "hello"}}));
                }
                Incoming::Note(_, p) => {
                    let u = &p["update"];
                    if u["sessionUpdate"] == "agent_message_chunk" {
                        text.push_str(u["content"]["text"].as_str().unwrap());
                    }
                    if u["sessionUpdate"] == "tool_call_update" && u["status"] == "completed" {
                        tool_done = true;
                    }
                }
                Incoming::Reply(3, result) => break result.unwrap(),
                Incoming::Reply(..) | Incoming::Gone(_) => {}
            }
        };
        assert_eq!(stop["stopReason"], "end_turn");
        assert!(tool_done, "the read ran");
        assert_eq!(text, "Looking.It says hello.");
        let first: Value = serde_json::from_str(&bodies.recv().unwrap()).unwrap();
        assert_eq!(first["stream"], true);
        assert_eq!(first["tools"].as_array().unwrap().len(), 6);
        let second: Value = serde_json::from_str(&bodies.recv().unwrap()).unwrap();
        let last = second["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        assert_eq!(
            (last["role"].as_str(), last["content"].as_str()),
            (Some("tool"), Some("hello"))
        );
        assert_eq!(last["tool_call_id"], "c1");
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn plan_mode_only_reads() {
        let (out, from) = channel();
        let link = Link {
            out,
            wake: Box::new(|| {}),
            waiting: Mutex::default(),
            next: AtomicU64::new(1),
            cancel: AtomicBool::new(false),
            commands_ok: AtomicBool::new(true),
            mode: Mutex::new("plan"),
        };
        let cwd = std::env::temp_dir().join(format!("ligase-plan-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).expect("dir");
        std::fs::write(cwd.join("a.txt"), "old").expect("write");
        let call = |name: &str, arguments: &str| Call {
            id: "c".into(),
            name: name.into(),
            arguments: arguments.into(),
        };
        let said = run_tool(
            &link,
            &cwd,
            &call("write_file", r#"{"path": "a.txt", "content": "new"}"#),
        );
        assert!(said.contains("plan mode"), "{said}");
        let said = run_tool(
            &link,
            &cwd,
            &call("run_command", r#"{"command": "echo hi"}"#),
        );
        assert!(said.contains("plan mode"), "{said}");
        assert_eq!(
            std::fs::read_to_string(cwd.join("a.txt")).expect("read"),
            "old"
        );
        // nothing was asked of the editor
        assert!(
            from.try_iter().all(|m| !matches!(m, Incoming::Request(..))),
            "plan mode asks nothing"
        );
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn compaction_keeps_the_newest_turn_whole() {
        // without a server, compact must not touch a short history
        let (out, _from) = channel();
        let link = Link {
            out,
            wake: Box::new(|| {}),
            waiting: Mutex::default(),
            next: AtomicU64::new(1),
            cancel: AtomicBool::new(false),
            commands_ok: AtomicBool::new(false),
            mode: Mutex::new("auto"),
        };
        let history = Mutex::new(vec![
            json!({"role": "system", "content": "s"}),
            json!({"role": "user", "content": "hi"}),
        ]);
        let config = Config {
            base: "http://localhost:9/v1".into(),
            model: "m".into(),
            key: None,
            context: 10,
        };
        assert!(compact(&link, &history, &config).is_ok());
        assert_eq!(history.lock().unwrap().len(), 2);
    }
}
