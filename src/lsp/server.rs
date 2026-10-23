//! one language server process. a reader thread parses its output, a writer
//! thread feeds its input, so a slow server never blocks the ui thread.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

use serde_json::{Value, json};

use super::{rpc, uri};

/// id of the `initialize` request.
pub const INIT: i64 = 0;

pub struct Server {
    child: Child,
    to_server: Sender<Vec<u8>>,
    pub from_server: Receiver<Value>,
    next_id: i64,
    /// `initialize` answered and `initialized` sent.
    pub ready: bool,
    /// notifications held until ready.
    waiting: Vec<Vec<u8>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    /// starts `command` in `root` and sends `initialize`. runs on the
    /// worker: starting a process can take a while. `wake` runs on the
    /// reader thread after each message.
    pub fn spawn(
        command: &str,
        args: &[&str],
        root: &Path,
        wake: impl Fn() + Send + 'static,
    ) -> Result<Server, String> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // no console window popping up for the server
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(|e| format!("{command}: {e}"))?;
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let mut stdout = child.stdout.take().ok_or("no stdout")?;

        let (to_server, outgoing) = channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("ligase-lsp-write".into())
            .spawn(move || {
                for bytes in outgoing {
                    if stdin
                        .write_all(&bytes)
                        .and_then(|()| stdin.flush())
                        .is_err()
                    {
                        return;
                    }
                }
            })
            .map_err(|e| e.to_string())?;

        let (incoming, from_server) = channel::<Value>();
        std::thread::Builder::new()
            .name("ligase-lsp-read".into())
            .spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 16 * 1024];
                loop {
                    let n = match stdout.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&chunk[..n]);
                    loop {
                        match rpc::take(&mut buf) {
                            Ok(Some(msg)) => {
                                if incoming.send(msg).is_err() {
                                    return;
                                }
                                wake();
                            }
                            Ok(None) => break,
                            // a broken stream can't be resynced
                            Err(_) => return,
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;

        let server = Server {
            child,
            to_server,
            from_server,
            next_id: INIT + 1,
            ready: false,
            waiting: Vec::new(),
        };
        let _ = server
            .to_server
            .send(rpc::request(INIT, "initialize", init_params(root)));
        Ok(server)
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// `initialize` came back: say `initialized` and send what waited.
    pub fn initialized(&mut self) {
        let _ = self
            .to_server
            .send(rpc::notification("initialized", json!({})));
        for bytes in self.waiting.drain(..) {
            let _ = self.to_server.send(bytes);
        }
        self.ready = true;
    }

    pub fn request(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let _ = self.to_server.send(rpc::request(id, method, params));
        id
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        let bytes = rpc::notification(method, params);
        if self.ready {
            let _ = self.to_server.send(bytes);
        } else {
            self.waiting.push(bytes);
        }
    }

    pub fn reply(&self, id: &Value, result: Value) {
        let _ = self.to_server.send(rpc::response(id, result));
    }
}

fn init_params(root: &Path) -> Value {
    let root_uri = uri::from_path(root);
    let name = root.file_name().map_or_else(
        || "workspace".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "ligase" },
        "rootUri": root_uri,
        "workspaceFolders": [{ "uri": root_uri, "name": name }],
        "capabilities": {
            "general": { "positionEncodings": ["utf-8", "utf-16"] },
            "textDocument": {
                "synchronization": { "didSave": false },
                "publishDiagnostics": { "versionSupport": true },
                "hover": { "contentFormat": ["plaintext", "markdown"] },
                "definition": { "linkSupport": true },
            },
            "workspace": { "configuration": true, "workspaceFolders": true },
        },
    })
}
