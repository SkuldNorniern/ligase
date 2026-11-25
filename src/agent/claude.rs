//! claude code without node: the `claude` program itself, spoken to in
//! its stream-json mode, turned into acp for the panel. runs in process
//! like the built-in agent; the program does the model calls and the tools.
//!
//! - in: `{"type":"user","message":{...}}` lines, and control requests
//!   (`initialize`, `interrupt`, `set_permission_mode`).
//! - out: `stream_event` deltas, whole `assistant` / `user` messages,
//!   `result` at the end of a turn, and `control_request`s asking to use a
//!   tool (`--permission-prompt-tool stdio`), answered with a
//!   `control_response`.
//! - edits claude makes on disk are read before and after, and handed to
//!   ligase as a write with the old text, so they can be looked over.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::acp::{Fail, Incoming};

const SESSION: &str = "claude";
/// longest line taken from the program; past it the line is dropped.
const MAX_LINE: usize = 8 * 1024 * 1024;
/// most of a tool's output shown in the panel.
const MAX_SHOWN: usize = 4000;
/// biggest file read to remember before an edit.
const MAX_BEFORE: u64 = 4 * 1024 * 1024;

/// claude's permission modes: id, name, what it lets through.
const MODES: [(&str, &str, &str); 4] = [
    ("default", "Ask first", "Asks before each edit or command"),
    (
        "acceptEdits",
        "Auto edits",
        "Edits files on its own, asks before commands",
    ),
    (
        "bypassPermissions",
        "Full auto",
        "Edits and runs anything without asking",
    ),
    ("plan", "Plan only", "Reads and plans, changes nothing"),
];

/// claude's model aliases: id, name, a line on it.
const MODELS: [(&str, &str, &str); 4] = [
    (
        "default",
        "Default",
        "The one Claude Code picks for the account",
    ),
    ("opus", "Opus", "Most capable, for hard work"),
    ("sonnet", "Sonnet", "Fast and capable, for everyday work"),
    ("haiku", "Haiku", "Fastest, for quick small things"),
];

/// tools that change files, and the input key naming the file.
const EDITS: [&str; 4] = ["Edit", "MultiEdit", "Write", "NotebookEdit"];

struct Link {
    out: Sender<Incoming>,
    wake: Box<dyn Fn() + Send + Sync>,
    waiting: Mutex<HashMap<String, Sender<Result<Value, String>>>>,
    next: AtomicU64,
    /// the program's stdin, while it runs.
    to_claude: Mutex<Option<ChildStdin>>,
    /// the prompt waiting for its turn to end.
    prompt: Mutex<Option<u64>>,
    cancelled: AtomicBool,
    /// text came as deltas for the message now going; its whole copy is
    /// not shown again.
    streamed: AtomicBool,
    /// tools allowed for the rest of the session.
    always: Mutex<HashSet<String>>,
    /// files as they were before an edit, by tool use.
    before: Mutex<HashMap<String, (PathBuf, String)>>,
    /// the tail of the program's stderr, to say why it stopped.
    said: Mutex<String>,
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

    /// asks ligase and waits for the answer.
    fn ask(&self, method: &str, params: Value, wait: Duration) -> Result<Value, String> {
        let id = format!("c{}", self.next.fetch_add(1, Ordering::Relaxed));
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

    /// one json line to the program.
    fn write(&self, line: &Value) -> bool {
        let Ok(mut slot) = self.to_claude.lock() else {
            return false;
        };
        let Some(stdin) = slot.as_mut() else {
            return false;
        };
        let mut text = line.to_string();
        text.push('\n');
        stdin
            .write_all(text.as_bytes())
            .and_then(|()| stdin.flush())
            .is_ok()
    }

    fn control(&self, request: Value) -> bool {
        let id = format!("l{}", self.next.fetch_add(1, Ordering::Relaxed));
        self.write(&json!({"type": "control_request", "request_id": id, "request": request}))
    }

    /// the prompt's turn is over.
    fn finish(&self, result: Result<Value, Fail>) {
        if let Some(id) = self.prompt.lock().ok().and_then(|mut p| p.take()) {
            self.send(Incoming::Reply(id, result));
        }
    }
}

fn modes_json(current: &str) -> Value {
    json!({
        "currentModeId": current,
        "availableModes": MODES.iter().map(|(id, name, about)| json!({"id": id, "name": name, "description": about})).collect::<Vec<_>>(),
    })
}

fn models_json(current: &str) -> Value {
    json!({
        "currentModelId": current,
        "availableModels": MODELS.iter().map(|(id, name, about)| json!({"modelId": id, "name": name, "description": about})).collect::<Vec<_>>(),
    })
}

/// runs the bridge on the lines ligase sends, until the pipe closes.
/// `program` is the `claude` found on PATH.
pub fn serve(
    program: (PathBuf, Vec<String>),
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
        to_claude: Mutex::default(),
        prompt: Mutex::default(),
        cancelled: AtomicBool::new(false),
        streamed: AtomicBool::new(false),
        always: Mutex::default(),
        before: Mutex::default(),
        said: Mutex::default(),
    });
    let mut child: Option<Child> = None;
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
                        "authMethods": [],
                    }))),
                    "session/new" => {
                        if child.is_none() {
                            match start(&program, &cwd, &link) {
                                Ok(c) => child = Some(c),
                                Err(why) => {
                                    reply(Err(Fail {
                                        code: -32603,
                                        message: why,
                                    }));
                                    continue;
                                }
                            }
                        }
                        reply(Ok(
                            json!({"sessionId": SESSION, "modes": modes_json("default"), "models": models_json("default")}),
                        ));
                    }
                    "session/set_mode" => {
                        let asked = message["params"]["modeId"].as_str().unwrap_or("");
                        match MODES.iter().find(|(m, ..)| *m == asked) {
                            Some((m, ..))
                                if link.control(
                                    json!({"subtype": "set_permission_mode", "mode": m}),
                                ) =>
                            {
                                reply(Ok(json!({})));
                                link.update(json!({"sessionUpdate": "current_mode_update", "currentModeId": m}));
                            }
                            _ => reply(Err(Fail {
                                code: -32602,
                                message: format!("no mode {asked}"),
                            })),
                        }
                    }
                    "session/set_model" => {
                        let asked = message["params"]["modelId"].as_str().unwrap_or("");
                        match MODELS.iter().find(|(m, ..)| *m == asked) {
                            Some((m, ..))
                                if link.control(json!({"subtype": "set_model", "model": m})) =>
                            {
                                reply(Ok(json!({})));
                            }
                            _ => reply(Err(Fail {
                                code: -32602,
                                message: format!("no model {asked}"),
                            })),
                        }
                    }
                    "session/prompt" => {
                        link.cancelled.store(false, Ordering::Relaxed);
                        if let Ok(mut p) = link.prompt.lock() {
                            *p = Some(id);
                        }
                        let content = prompt_content(&message["params"]["prompt"], &cwd);
                        let sent = link.write(&json!({
                            "type": "user",
                            "message": {"role": "user", "content": content},
                            "parent_tool_use_id": null,
                            "session_id": SESSION,
                        }));
                        if !sent {
                            let why = stopped(&link);
                            link.finish(Err(Fail {
                                code: -32603,
                                message: why,
                            }));
                        }
                    }
                    _ => reply(Err(Fail {
                        code: -32601,
                        message: format!("{method} is not supported"),
                    })),
                }
            }
            (Some("session/cancel"), None) => {
                link.cancelled.store(true, Ordering::Relaxed);
                link.control(json!({"subtype": "interrupt"}));
            }
            (None, Some(Value::String(id))) => {
                let result = match message.get("error") {
                    Some(e) => Err(e["message"].as_str().unwrap_or("failed").to_owned()),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                if let Some(tx) = link.waiting.lock().ok().and_then(|mut w| w.remove(&id)) {
                    let _ = tx.send(result);
                }
            }
            _ => {}
        }
    }
    // the panel let go: stdin closes, the program gets a moment, then goes
    if let Ok(mut s) = link.to_claude.lock() {
        s.take();
    }
    if let Some(mut c) = child {
        for _ in 0..40 {
            if !matches!(c.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// what stopped the program, from its stderr, or that it did.
fn stopped(link: &Link) -> String {
    let said = link.said.lock().map(|s| s.clone()).unwrap_or_default();
    said.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map_or_else(
            || "claude stopped".to_owned(),
            |l| l.chars().take(300).collect(),
        )
}

/// starts `claude` in stream-json mode with its readers.
fn start(program: &(PathBuf, Vec<String>), cwd: &Path, link: &Arc<Link>) -> Result<Child, String> {
    let mut cmd = Command::new(&program.0);
    cmd.args(&program.1)
        .args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            "--permission-mode",
            "default",
        ])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| format!("claude: {e}"))?;
    let (Some(stdin), Some(stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let _ = child.kill();
        return Err("claude: no pipes".to_owned());
    };
    if let Ok(mut s) = link.to_claude.lock() {
        *s = Some(stdin);
    }
    let tail = Arc::clone(link);
    std::thread::spawn(move || {
        let mut chunk = [0u8; 1024];
        while let Ok(n) = stderr.read(&mut chunk) {
            if n == 0 {
                break;
            }
            if let Ok(mut t) = tail.said.lock() {
                t.push_str(&String::from_utf8_lossy(&chunk[..n]));
                if t.len() > 4096 {
                    let mut cut = t.len() - 4096;
                    while !t.is_char_boundary(cut) {
                        cut += 1;
                    }
                    t.drain(..cut);
                }
            }
        }
    });
    let reader = Arc::clone(link);
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match (&mut lines)
                .take(MAX_LINE as u64)
                .read_until(b'\n', &mut buf)
            {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if let Ok(v) = serde_json::from_slice::<Value>(&buf) {
                handle(&reader, &v);
            }
        }
        // the program went: a turn still waiting ends with why
        let why = stopped(&reader);
        reader.finish(Err(Fail {
            code: -32603,
            message: why,
        }));
        if let Ok(mut s) = reader.to_claude.lock() {
            s.take();
        }
    });
    link.control(json!({"subtype": "initialize"}));
    Ok(child)
}

/// the acp prompt as claude message content: text, and files named by path.
fn prompt_content(prompt: &Value, cwd: &Path) -> Value {
    let parts: Vec<Value> = prompt
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| match block["type"].as_str()? {
            "text" => Some(json!({"type": "text", "text": block["text"].as_str()?})),
            "resource_link" => {
                let uri = block["uri"].as_str()?;
                let path = crate::lsp::uri::to_path(uri.split('#').next().unwrap_or(uri))?;
                let shown = path
                    .strip_prefix(cwd)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                let lines = uri
                    .split_once("#L")
                    .map(|(_, l)| format!(" (lines {l})"))
                    .unwrap_or_default();
                Some(json!({"type": "text", "text": format!("@{shown}{lines}")}))
            }
            _ => None,
        })
        .collect();
    Value::Array(parts)
}

/// one line from the program.
fn handle(link: &Arc<Link>, v: &Value) {
    match v["type"].as_str() {
        Some("stream_event") => {
            let event = &v["event"];
            match (event["type"].as_str(), event["delta"]["type"].as_str()) {
                (Some("content_block_delta"), Some("text_delta")) => {
                    link.streamed.store(true, Ordering::Relaxed);
                    link.update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": event["delta"]["text"]}}));
                }
                (Some("content_block_delta"), Some("thinking_delta")) => {
                    link.update(json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": event["delta"]["thinking"]}}));
                }
                (Some("message_start"), _) => link.streamed.store(false, Ordering::Relaxed),
                _ => {}
            }
        }
        Some("assistant") => {
            let streamed = link.streamed.swap(false, Ordering::Relaxed);
            for block in v["message"]["content"].as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("text") if !streamed => {
                        link.update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": block["text"]}}));
                    }
                    Some("tool_use") => {
                        let name = block["name"].as_str().unwrap_or("tool");
                        let input = &block["input"];
                        let mut update = json!({
                            "sessionUpdate": "tool_call",
                            "toolCallId": block["id"],
                            "title": title(name, input),
                            "kind": kind(name),
                            "status": "pending",
                        });
                        if let Some(path) = input["file_path"]
                            .as_str()
                            .or(input["notebook_path"].as_str())
                        {
                            update["locations"] = json!([{"path": path}]);
                        }
                        link.update(update);
                    }
                    _ => {}
                }
            }
        }
        Some("user") => {
            for block in v["message"]["content"].as_array().into_iter().flatten() {
                if block["type"].as_str() != Some("tool_result") {
                    continue;
                }
                let id = block["tool_use_id"].as_str().unwrap_or("");
                let failed = block["is_error"].as_bool().unwrap_or(false);
                let text = result_text(&block["content"]);
                link.update(json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": id,
                    "status": if failed { "failed" } else { "completed" },
                    "content": [{"type": "content", "content": {"type": "text", "text": cut(&text, MAX_SHOWN)}}],
                }));
                // an edit went to disk: ligase gets the file and its old text
                let before = link.before.lock().ok().and_then(|mut b| b.remove(id));
                if let (Some((path, old)), false) = (before, failed) {
                    let link = Arc::clone(link);
                    std::thread::spawn(move || {
                        let Ok(now) = std::fs::read_to_string(&path) else {
                            return;
                        };
                        if now != old {
                            let _ = link.ask(
                                "fs/write_text_file",
                                json!({"sessionId": SESSION, "path": path, "content": now, "_meta": {"before": old}}),
                                Duration::from_secs(60),
                            );
                        }
                    });
                }
            }
        }
        Some("result") => {
            let cancelled = link.cancelled.swap(false, Ordering::Relaxed);
            let failed = v["is_error"].as_bool().unwrap_or(false);
            let result = if cancelled {
                Ok(json!({"stopReason": "cancelled"}))
            } else if failed {
                let why = v["result"]
                    .as_str()
                    .or(v["subtype"].as_str())
                    .unwrap_or("claude failed");
                Err(Fail {
                    code: -32603,
                    message: why.chars().take(500).collect(),
                })
            } else {
                Ok(json!({"stopReason": "end_turn"}))
            };
            link.finish(result);
        }
        Some("control_request") => {
            let request_id = v["request_id"].clone();
            let request = v["request"].clone();
            let link = Arc::clone(link);
            // asking the person can take a while; the reader goes on
            std::thread::spawn(move || {
                let response = match request["subtype"].as_str() {
                    Some("can_use_tool") => json!({
                        "subtype": "success",
                        "request_id": request_id,
                        "response": permit(&link, &request),
                    }),
                    _ => json!({
                        "subtype": "error",
                        "request_id": request_id,
                        "error": "not supported by ligase",
                    }),
                };
                link.write(&json!({"type": "control_response", "response": response}));
            });
        }
        _ => {}
    }
}

/// asks the person about a tool, unless it was allowed for the session;
/// remembers a file's text before an edit to it.
fn permit(link: &Link, request: &Value) -> Value {
    let name = request["tool_name"].as_str().unwrap_or("tool");
    let input = &request["input"];
    let tool_id = request["tool_use_id"].as_str().unwrap_or("").to_owned();
    let allowed = link.always.lock().is_ok_and(|a| a.contains(name));
    let answer = if allowed {
        "once".to_owned()
    } else {
        link.ask(
            "session/request_permission",
            json!({
                "sessionId": SESSION,
                "toolCall": {"toolCallId": tool_id, "title": title(name, input), "kind": kind(name)},
                "options": [
                    {"optionId": "once", "name": "Allow", "kind": "allow_once"},
                    {"optionId": "always", "name": "Always allow", "kind": "allow_always"},
                    {"optionId": "no", "name": "Don't", "kind": "reject_once"},
                ],
            }),
            Duration::from_secs(60 * 60),
        )
        .ok()
        .and_then(|a| a["outcome"]["optionId"].as_str().map(str::to_owned))
        .unwrap_or_default()
    };
    match answer.as_str() {
        "once" | "always" => {
            if answer == "always"
                && let Ok(mut a) = link.always.lock()
            {
                a.insert(name.to_owned());
            }
            if EDITS.contains(&name)
                && let Some(path) = input["file_path"]
                    .as_str()
                    .or(input["notebook_path"].as_str())
            {
                let path = PathBuf::from(path);
                let old = std::fs::metadata(&path)
                    .ok()
                    .filter(|m| m.len() <= MAX_BEFORE)
                    .and_then(|_| std::fs::read_to_string(&path).ok())
                    .unwrap_or_default();
                if let Ok(mut b) = link.before.lock() {
                    b.insert(tool_id, (path, old));
                }
            }
            json!({"behavior": "allow", "updatedInput": input})
        }
        _ => json!({"behavior": "deny", "message": "the person did not allow it"}),
    }
}

/// a tool use said short.
fn title(name: &str, input: &Value) -> String {
    let s = |k: &str| input[k].as_str().unwrap_or("");
    let file = |k: &str| {
        Path::new(s(k))
            .file_name()
            .map_or_else(|| s(k).to_owned(), |n| n.to_string_lossy().into_owned())
    };
    match name {
        "Read" => format!("Read {}", file("file_path")),
        "Edit" | "MultiEdit" => format!("Edit {}", file("file_path")),
        "Write" => format!("Write {}", file("file_path")),
        "NotebookEdit" => format!("Edit {}", file("notebook_path")),
        "Bash" => format!("Run {}", cut(s("command"), 120)),
        "Grep" => format!("Search for {}", s("pattern")),
        "Glob" => format!("Find {}", s("pattern")),
        "WebFetch" => format!("Fetch {}", s("url")),
        "WebSearch" => format!("Search the web for {}", s("query")),
        "Task" => format!("Agent: {}", cut(s("description"), 80)),
        _ => name.to_owned(),
    }
}

fn kind(name: &str) -> &'static str {
    match name {
        "Read" => "read",
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => "edit",
        "Bash" => "execute",
        "Grep" | "Glob" | "WebSearch" => "search",
        "WebFetch" => "fetch",
        _ => "other",
    }
}

/// a tool result's text: a string, or text blocks.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(list) => list
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `text` cut on a char edge to `max` bytes.
fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_link() -> (Arc<Link>, Receiver<Incoming>) {
        let (out, rx) = channel();
        let link = Arc::new(Link {
            out,
            wake: Box::new(|| {}),
            waiting: Mutex::default(),
            next: AtomicU64::new(1),
            to_claude: Mutex::default(),
            prompt: Mutex::new(Some(7)),
            cancelled: AtomicBool::new(false),
            streamed: AtomicBool::new(false),
            always: Mutex::default(),
            before: Mutex::default(),
            said: Mutex::default(),
        });
        (link, rx)
    }

    fn updates(rx: &Receiver<Incoming>) -> Vec<Value> {
        rx.try_iter()
            .filter_map(|m| match m {
                Incoming::Note(_, p) => Some(p["update"].clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn deltas_show_once_and_tools_become_calls() {
        let (link, rx) = test_link();
        handle(
            &link,
            &json!({"type": "stream_event", "event": {"type": "message_start"}}),
        );
        handle(
            &link,
            &json!({"type": "stream_event", "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "hi"}}}),
        );
        handle(
            &link,
            &json!({"type": "assistant", "message": {"content": [
                {"type": "text", "text": "hi"},
                {"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": "/w/a.rs"}}
            ]}}),
        );
        handle(
            &link,
            &json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "ok"}]}
            ]}}),
        );
        let u = updates(&rx);
        let texts = u
            .iter()
            .filter(|u| u["sessionUpdate"] == "agent_message_chunk")
            .count();
        assert_eq!(texts, 1, "the whole message doesn't repeat the deltas");
        assert!(u.iter().any(|u| u["sessionUpdate"] == "tool_call"
            && u["title"] == "Edit a.rs"
            && u["kind"] == "edit"));
        assert!(
            u.iter()
                .any(|u| u["sessionUpdate"] == "tool_call_update" && u["status"] == "completed")
        );
    }

    #[test]
    fn a_result_ends_the_turn() {
        let (link, rx) = test_link();
        handle(
            &link,
            &json!({"type": "result", "subtype": "success", "is_error": false}),
        );
        let replies: Vec<_> = rx
            .try_iter()
            .filter_map(|m| match m {
                Incoming::Reply(id, r) => Some((id, r.map(|v| v["stopReason"].clone()).ok())),
                _ => None,
            })
            .collect();
        assert_eq!(replies, vec![(7, Some(json!("end_turn")))]);
        // a failed one says why
        let (link, rx) = test_link();
        handle(
            &link,
            &json!({"type": "result", "is_error": true, "result": "Invalid API key · Please run /login"}),
        );
        let failed = rx
            .try_iter()
            .any(|m| matches!(m, Incoming::Reply(7, Err(f)) if f.message.contains("/login")));
        assert!(failed);
    }

    #[test]
    fn prompts_name_files_by_path() {
        let cwd = PathBuf::from(if cfg!(windows) { "C:\\w" } else { "/w" });
        let uri = if cfg!(windows) {
            "file:///C:/w/src/a.rs#L3-9"
        } else {
            "file:///w/src/a.rs#L3-9"
        };
        let content = prompt_content(
            &json!([{"type": "text", "text": "fix"}, {"type": "resource_link", "uri": uri, "name": "a.rs"}]),
            &cwd,
        );
        assert_eq!(content[0]["text"], "fix");
        let named = content[1]["text"].as_str().unwrap_or("").replace('\\', "/");
        assert_eq!(named, "@src/a.rs (lines 3-9)");
    }
}
