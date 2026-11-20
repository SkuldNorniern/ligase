//! the agent client protocol's pipe: json-rpc 2.0, one message a line, on
//! the agent program's stdin and stdout. a thread reads, one writes, one
//! keeps the last of stderr for when it fails. what comes in is untrusted:
//! a line past the cap ends the pipe, nothing panics.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// longest line taken from the agent: a big file read back fits.
const MAX_LINE: usize = 32 * 1024 * 1024;
/// stderr lines kept to say why it stopped.
const ERR_LINES: usize = 20;

/// what came from the agent.
#[derive(Debug)]
pub enum Incoming {
    /// the answer to a request we sent.
    Reply(u64, Result<Value, Fail>),
    /// the agent asks us something and waits: `fs/...`, permission.
    Request(Value, String, Value),
    /// the agent tells us something: `session/update`.
    Note(String, Value),
    /// the program ended or its output broke; what stderr said last.
    Gone(String),
}

/// a json-rpc error.
#[derive(Clone, Debug, PartialEq)]
pub struct Fail {
    pub code: i64,
    pub message: String,
}

pub struct Pipe {
    child: Child,
    to_agent: Sender<String>,
    pub from_agent: Receiver<Incoming>,
    next: u64,
}

/// how long a closed agent gets to finish before it is killed.
const GRACE: std::time::Duration = std::time::Duration::from_secs(2);

impl Drop for Pipe {
    /// closes the agent's stdin, its sign to finish, and kills it only when
    /// it doesn't within [`GRACE`]. waits: drop it off the ui thread.
    fn drop(&mut self) {
        let (closed, _) = channel();
        drop(std::mem::replace(&mut self.to_agent, closed));
        let started = std::time::Instant::now();
        while started.elapsed() < GRACE {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Pipe {
    /// starts `program` with `args` in `cwd`. `wake` runs on the reader
    /// thread after each message. runs on a worker: starting may wait.
    pub fn spawn(
        program: &Path,
        args: &[String],
        cwd: &Path,
        wake: impl Fn() + Send + 'static,
    ) -> Result<Self, String> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("{}: {e}", program.display()))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            return Err("the agent's pipes did not open".to_owned());
        };
        let said: Arc<Mutex<VecDeque<String>>> = Arc::default();
        let tail = Arc::clone(&said);
        std::thread::Builder::new()
            .name("ligase-agent-err".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if let Ok(mut t) = tail.lock() {
                        if t.len() >= ERR_LINES {
                            t.pop_front();
                        }
                        t.push_back(line.chars().take(400).collect());
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let (to_agent, outgoing) = channel::<String>();
        std::thread::Builder::new()
            .name("ligase-agent-out".into())
            .spawn(move || {
                let mut stdin = stdin;
                for line in outgoing {
                    if stdin.write_all(line.as_bytes()).is_err()
                        || stdin.write_all(b"\n").is_err()
                        || stdin.flush().is_err()
                    {
                        return;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let (incoming, from_agent) = channel();
        std::thread::Builder::new()
            .name("ligase-agent-in".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                let mut line = Vec::new();
                loop {
                    line.clear();
                    match read_line(&mut reader, &mut line) {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(why) => {
                            let _ = incoming.send(Incoming::Gone(why));
                            wake();
                            return;
                        }
                    }
                    if let Some(message) = parse(&line)
                        && incoming.send(message).is_err()
                    {
                        return;
                    }
                    wake();
                }
                let last = said
                    .lock()
                    .map(|t| t.iter().cloned().collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default();
                let _ = incoming.send(Incoming::Gone(last));
                wake();
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            child,
            to_agent,
            from_agent,
            next: 1,
        })
    }

    /// sends a request; its reply comes back with the id given.
    pub fn request(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// answers the agent's request `id`.
    pub fn reply(&self, id: &Value, result: Result<Value, Fail>) {
        let message = match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(fail) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": fail.code, "message": fail.message},
            }),
        };
        self.send(&message);
    }

    fn send(&self, message: &Value) {
        // serde writes no raw newlines: one message stays one line
        let _ = self.to_agent.send(message.to_string());
    }
}

/// one line into `line`, without its ending. false at the end of the
/// stream; an error past [`MAX_LINE`].
fn read_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> Result<bool, String> {
    let read = reader
        .by_ref()
        .take(MAX_LINE as u64 + 1)
        .read_until(b'\n', line)
        .map_err(|e| e.to_string())?;
    if read == 0 {
        return Ok(false);
    }
    if line.len() > MAX_LINE {
        return Err("the agent sent a line past the size cap".to_owned());
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    Ok(true)
}

/// a line as a message; none for anything that isn't json-rpc (some
/// agents print a banner first).
fn parse(line: &[u8]) -> Option<Incoming> {
    let value: Value = serde_json::from_slice(line).ok()?;
    let method = value.get("method").and_then(Value::as_str);
    let id = value.get("id");
    match (method, id) {
        (Some(method), Some(id)) => Some(Incoming::Request(
            id.clone(),
            method.to_owned(),
            value.get("params").cloned().unwrap_or(Value::Null),
        )),
        (Some(method), None) => Some(Incoming::Note(
            method.to_owned(),
            value.get("params").cloned().unwrap_or(Value::Null),
        )),
        (None, Some(id)) => {
            let id = id.as_u64()?;
            let result = match value.get("error") {
                Some(error) => Err(Fail {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("the agent failed")
                        .chars()
                        .take(2000)
                        .collect(),
                }),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            };
            Some(Incoming::Reply(id, result))
        }
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_told_apart() {
        let reply = parse(br#"{"jsonrpc":"2.0","id":3,"result":{"sessionId":"s"}}"#);
        assert!(matches!(reply, Some(Incoming::Reply(3, Ok(_)))));
        let fail = parse(br#"{"jsonrpc":"2.0","id":4,"error":{"code":-32000,"message":"auth"}}"#);
        assert!(matches!(
            fail,
            Some(Incoming::Reply(4, Err(Fail { code: -32000, .. })))
        ));
        let ask = parse(br#"{"jsonrpc":"2.0","id":"a","method":"fs/read_text_file","params":{}}"#);
        assert!(matches!(ask, Some(Incoming::Request(_, ref m, _)) if m == "fs/read_text_file"));
        let note = parse(br#"{"jsonrpc":"2.0","method":"session/update","params":{"x":1}}"#);
        assert!(matches!(note, Some(Incoming::Note(ref m, _)) if m == "session/update"));
        assert!(parse(b"Starting agent...").is_none());
        assert!(parse(b"{}").is_none());
        assert!(
            parse(br#"{"id":"text","result":1}"#).is_none(),
            "our ids are numbers"
        );
    }

    #[test]
    fn lines_are_capped_and_trimmed() {
        let mut input: &[u8] = b"one\r\ntwo\nlast";
        let mut line = Vec::new();
        assert_eq!(read_line(&mut input, &mut line), Ok(true));
        assert_eq!(line, b"one");
        line.clear();
        assert_eq!(read_line(&mut input, &mut line), Ok(true));
        assert_eq!(line, b"two");
        line.clear();
        assert_eq!(read_line(&mut input, &mut line), Ok(true));
        assert_eq!(line, b"last");
        line.clear();
        assert_eq!(read_line(&mut input, &mut line), Ok(false));
    }
}
