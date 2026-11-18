//! one shell on another machine over ssh, kept open, asked one command at a time.

// used by the remote workspace, next
#![allow(dead_code)]

pub mod browse;

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::terminal;

/// most bytes read back from one command.
pub const MAX_REPLY: usize = 16 * 1024 * 1024;
/// a file bigger than this isn't read.
pub const MAX_FILE: u64 = 8 * 1024 * 1024;

const MAX_ENTRIES: usize = 50_000;
const MAX_PATH: usize = 4096;
/// longest piece the reader hands over at once, so one long line can't fill memory.
const LINE_CHUNK: u64 = 1024 * 1024;
const QUEUE: usize = 256;
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const LINK_DOWN: &str = "the link is down";

pub struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    counter: u64,
    broken: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
}

impl Session {
    /// `ssh -T -- host sh` (no window on windows: CREATE_NO_WINDOW). host must pass crate::terminal::host_ok.
    pub fn connect(host: &str) -> Result<Session, String> {
        if !terminal::host_ok(host) {
            return Err(format!("not a host name: {host}"));
        }
        // no prompts: nobody can answer them on a hidden link. keys or an
        // agent log in; a password-only host fails fast instead of hanging
        Session::spawn(
            "ssh",
            &[
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "--",
                host,
                "sh",
            ],
        )
    }

    /// any program speaking sh on stdin; for tests (e.g. "sh" from git for windows) and for connect.
    pub fn spawn(program: &str, args: &[&str]) -> Result<Session, String> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot start {program}: {e}"))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("no pipes to {program}"));
        };
        let (tx, lines) = mpsc::sync_channel(QUEUE);
        let reader = thread::Builder::new()
            .name("remote-reader".into())
            .spawn(move || read_lines(stdout, tx));
        if let Err(e) = reader {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("cannot start reader: {e}"));
        }
        Ok(Session {
            child,
            stdin: Some(stdin),
            lines,
            counter: 0,
            broken: false,
        })
    }

    /// runs `script` and gives its stdout and exit status. waits at most `timeout`.
    /// the last newline of the output is dropped.
    pub fn run(&mut self, script: &str, timeout: Duration) -> Result<(String, i32), String> {
        if self.broken {
            return Err(LINK_DOWN.into());
        }
        let mark = self.mark();
        let cmd = format!(
            "printf '\\n%s\\n' '{mark}'; {{ {script}\n}} 2>/dev/null; printf '\\n%s %s\\n' '{mark}' \"$?\"\n"
        );
        self.send(&cmd)?;

        let deadline = Instant::now() + timeout;
        let mut out = String::new();
        let mut begun = false;
        let status = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => return Err(self.fail("timed out")),
                Err(RecvTimeoutError::Disconnected) => return Err(self.fail("the link closed")),
            };
            if !begun {
                begun = line.trim_end_matches('\n') == mark.as_str();
                continue;
            }
            if let Some(rest) = line.strip_prefix(mark.as_str()) {
                match rest.trim().parse::<i32>() {
                    Ok(code) => break code,
                    Err(_) => return Err(self.fail("bad reply")),
                }
            }
            if out.len() + line.len() > MAX_REPLY {
                return Err(self.fail("reply too big"));
            }
            out.push_str(&line);
        };
        // one newline from the printf before the end line, one from the script
        if out.ends_with('\n') {
            out.pop();
        }
        if out.ends_with('\n') {
            out.pop();
        }
        Ok((out, status))
    }

    /// the entries of `dir` (absolute posix path): `ls -1Ap` style, '/' suffix = dir; `.`/`..` never. sorted dirs first then name.
    pub fn list(&mut self, dir: &str) -> Result<Vec<Entry>, String> {
        let script = format!("(cd -- {} && ls -1Ap)", quoted(dir)?);
        let (out, status) = self.run(&script, CALL_TIMEOUT)?;
        if status != 0 {
            return Err(format!("cannot list {dir}"));
        }
        let mut entries = Vec::new();
        for line in out.lines() {
            let (name, is_dir) = match line.strip_suffix('/') {
                Some(name) => (name, true),
                None => (line, false),
            };
            if name.is_empty() || name == "." || name == ".." {
                continue;
            }
            if entries.len() == MAX_ENTRIES {
                return Err(format!("too many entries in {dir}"));
            }
            entries.push(Entry {
                name: name.to_string(),
                dir: is_dir,
            });
        }
        entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.cmp(&b.name)));
        Ok(entries)
    }

    /// the bytes of `path`; refuses files over MAX_FILE (check size first with `wc -c < file`). transfers as base64 (`base64 < 'path'`), decoded here.
    pub fn read(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let q = quoted(path)?;
        let (size, status) = self.run(&format!("wc -c < {q}"), CALL_TIMEOUT)?;
        if status != 0 {
            return Err(format!("cannot read {path}"));
        }
        let size: u64 = size
            .trim()
            .parse()
            .map_err(|_| format!("cannot size {path}"))?;
        if size > MAX_FILE {
            return Err(format!("file too big: {size} bytes"));
        }
        let (text, status) = self.run(&format!("base64 < {q}"), CALL_TIMEOUT)?;
        if status != 0 {
            return Err(format!("cannot read {path}"));
        }
        let bytes = decode(&text)?;
        if bytes.len() as u64 > MAX_FILE {
            return Err(format!("file too big: {path}"));
        }
        Ok(bytes)
    }

    /// writes `bytes` to `path` atomically there: base64 into a temp file next to it (`path.ligase-<n>.tmp`), then `mv -f`. the temp file is removed on failure.
    pub fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let q = quoted(path)?;
        if bytes.len() as u64 > MAX_FILE {
            return Err(format!("file too big: {} bytes", bytes.len()));
        }
        let tok = self.token();
        let tmp = format!("{path}.ligase-{tok}.tmp");
        let qt = quote(&tmp).ok_or_else(|| "bad path".to_string())?;
        let delim = format!("__LIGASE_EOF_{tok}");

        let text = encode(bytes);
        let mut script = format!("if base64 -d > {qt} <<'{delim}'\n");
        for start in (0..text.len()).step_by(76) {
            let end = (start + 76).min(text.len());
            // base64 output is ascii, so byte slices are char boundaries
            script.push_str(&text[start..end]);
            script.push('\n');
        }
        script.push_str(&format!(
            "{delim}\nthen mv -f {qt} {q} || {{ rm -f {qt}; false; }}; else rm -f {qt}; false; fi\n"
        ));

        let (_, status) = self.run(&script, CALL_TIMEOUT)?;
        if status != 0 {
            return Err(format!("cannot write {path}"));
        }
        Ok(())
    }

    /// the remote home directory (`printf %s "$HOME"`).
    pub fn home(&mut self) -> Result<String, String> {
        let (out, status) = self.run("printf %s \"$HOME\"", CALL_TIMEOUT)?;
        if status != 0 || out.is_empty() {
            return Err("no home directory".into());
        }
        Ok(out)
    }

    fn send(&mut self, text: &str) -> Result<(), String> {
        let result = match self.stdin.as_mut() {
            Some(stdin) => stdin
                .write_all(text.as_bytes())
                .and_then(|()| stdin.flush()),
            None => Err(std::io::Error::other("stdin closed")),
        };
        match result {
            Ok(()) => Ok(()),
            Err(_) => Err(self.fail(LINK_DOWN)),
        }
    }

    /// marks the session broken and gives the message back.
    fn fail(&mut self, msg: &str) -> String {
        self.broken = true;
        msg.to_string()
    }

    fn token(&mut self) -> String {
        self.counter += 1;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        format!("{nanos:x}_{}", self.counter)
    }

    fn mark(&mut self) -> String {
        format!("__LIGASE_{}__", self.token())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // close stdin, then kill: a stuck remote must not hang the app
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_lines(stdout: ChildStdout, tx: SyncSender<String>) {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match (&mut reader).take(LINE_CHUNK).read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if tx.send(String::from_utf8_lossy(&buf).into_owned()).is_err() {
            return;
        }
    }
}

/// `s` as one single-quoted sh word: `'` becomes `'\''`. rejects NUL.
pub fn quote(s: &str) -> Option<String> {
    if s.contains('\0') {
        return None;
    }
    Some(format!("'{}'", s.replace('\'', "'\\''")))
}

/// a path that may go to the remote shell, quoted.
fn quoted(path: &str) -> Result<String, String> {
    if !path.starts_with('/') || path.contains('\0') || path.len() > MAX_PATH {
        return Err("bad path".into());
    }
    quote(path).ok_or_else(|| "bad path".to_string())
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// standard base64 with padding.
fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn value(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// decodes base64, ignoring whitespace. bad characters or a broken length are errors.
fn decode(text: &str) -> Result<Vec<u8>, String> {
    let bad = || "bad base64".to_string();
    let mut vals = Vec::with_capacity(text.len());
    let mut pad = 0usize;
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            pad += 1;
            continue;
        }
        if pad > 0 {
            return Err(bad());
        }
        vals.push(value(c).ok_or_else(bad)?);
    }
    if pad > 2 || !(vals.len() + pad).is_multiple_of(4) {
        return Err(bad());
    }
    let mut out = Vec::with_capacity(vals.len() / 4 * 3 + 3);
    for quad in vals.chunks(4) {
        if quad.len() == 1 {
            return Err(bad());
        }
        let n = quad
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, v)| acc | (u32::from(*v) << (18 - 6 * i)));
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..quad.len() - 1]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const T: Duration = Duration::from_secs(20);

    #[test]
    fn quote_plain() {
        assert_eq!(quote("/a b/c").as_deref(), Some("'/a b/c'"));
    }

    #[test]
    fn quote_single_quote() {
        assert_eq!(quote("it's").as_deref(), Some("'it'\\''s'"));
    }

    #[test]
    fn quote_rejects_nul() {
        assert_eq!(quote("a\0b"), None);
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_round_trip_short() {
        for n in 0..6u8 {
            let data: Vec<u8> = (0..n)
                .map(|i| i.wrapping_mul(37).wrapping_add(200))
                .collect();
            assert_eq!(decode(&encode(&data)).unwrap(), data);
        }
    }

    #[test]
    fn base64_round_trip_big() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let data: Vec<u8> = (0..1000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect();
        assert_eq!(decode(&encode(&data)).unwrap(), data);
    }

    #[test]
    fn base64_skips_whitespace() {
        assert_eq!(decode("Zm9v\nYmFy\r\n").unwrap(), b"foobar");
        assert_eq!(decode(" Zm8= \n").unwrap(), b"fo");
    }

    #[test]
    fn base64_rejects_bad_input() {
        assert!(decode("Zm9*").is_err());
        assert!(decode("Zg=").is_err());
        assert!(decode("Z").is_err());
        assert!(decode("Zg==Zg==").is_err());
    }

    #[test]
    fn connect_checks_host() {
        assert!(Session::connect("-oProxyCommand=x").is_err());
        assert!(Session::connect("").is_err());
    }

    /// a posix sh, or None (the tests then skip).
    fn sh() -> Option<&'static str> {
        ["sh", r"C:\Program Files\Git\usr\bin\sh.exe", "/usr/bin/sh"]
            .into_iter()
            .find(|p| {
                Command::new(p)
                    .args(["-c", "true"])
                    .status()
                    .is_ok_and(|s| s.success())
            })
    }

    fn session() -> Option<Session> {
        let program = sh();
        if program.is_none() {
            eprintln!("no sh found, remote tests skipped");
        }
        Session::spawn(program?, &["-s"]).ok()
    }

    /// `C:\x\y` as sh sees it: `/c/x/y`.
    fn posix(p: &Path) -> String {
        let s = p.to_string_lossy().replace('\\', "/");
        let b = s.as_bytes();
        if b.len() > 2 && b[1] == b':' {
            format!("/{}{}", (b[0] as char).to_ascii_lowercase(), &s[2..])
        } else {
            s
        }
    }

    /// a fresh empty dir under the temp dir, named per test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ligase-remote-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn run_output_and_status() {
        let Some(mut s) = session() else { return };
        assert_eq!(s.run("echo hi", T).unwrap(), ("hi".to_string(), 0));
        assert_eq!(s.run("echo err >&2; echo ok", T).unwrap().0, "ok");
        assert_eq!(
            s.run("exit_status_test() { return 3; }; exit_status_test", T)
                .unwrap()
                .1,
            3
        );
        assert_eq!(
            s.run("echo '__LIGASE_nope__ 9'", T).unwrap(),
            ("__LIGASE_nope__ 9".to_string(), 0)
        );
        assert_eq!(s.run("echo still", T).unwrap().0, "still");
    }

    #[test]
    fn list_entries() {
        let Some(mut s) = session() else { return };
        let dir = scratch("list");
        fs::write(dir.join("b.txt"), b"x").unwrap();
        fs::write(dir.join("it's.txt"), b"x").unwrap();
        fs::write(dir.join(".hidden"), b"x").unwrap();
        fs::create_dir(dir.join("a dir")).unwrap();
        let got = s.list(&posix(&dir)).unwrap();
        assert_eq!(
            got,
            vec![
                Entry {
                    name: "a dir".into(),
                    dir: true
                },
                Entry {
                    name: ".hidden".into(),
                    dir: false
                },
                Entry {
                    name: "b.txt".into(),
                    dir: false
                },
                Entry {
                    name: "it's.txt".into(),
                    dir: false
                },
            ]
        );
        assert!(s.list(&posix(&dir.join("missing"))).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_then_read_bytes() {
        let Some(mut s) = session() else { return };
        let dir = scratch("bytes");
        let path = posix(&dir.join("it's.bin"));
        let data: Vec<u8> = vec![0, 1, 0x80, 0xff, b'\'', b'\n', 0, b'z'];
        s.write(&path, &data).unwrap();
        assert_eq!(s.read(&path).unwrap(), data);
        let second = b"second\n".to_vec();
        s.write(&path, &second).unwrap();
        assert_eq!(s.read(&path).unwrap(), second);
        s.write(&path, &[]).unwrap();
        assert_eq!(s.read(&path).unwrap(), Vec::<u8>::new());
        let names: Vec<String> = s
            .list(&posix(&dir))
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["it's.bin".to_string()]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_refuses_missing_and_big() {
        let Some(mut s) = session() else { return };
        let dir = scratch("refuse");
        assert!(s.read(&posix(&dir.join("missing"))).is_err());
        let big = dir.join("big.bin");
        fs::write(&big, vec![0u8; MAX_FILE as usize + 1]).unwrap();
        assert!(s.read(&posix(&big)).is_err());
        assert!(s.home().is_ok());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn home_is_absolute() {
        let Some(mut s) = session() else { return };
        assert!(s.home().unwrap().starts_with('/'));
    }

    #[test]
    fn timeout_breaks_link() {
        let Some(mut s) = session() else { return };
        assert_eq!(
            s.run("sleep 5", Duration::from_millis(300)),
            Err("timed out".to_string())
        );
        let started = Instant::now();
        assert_eq!(s.run("echo hi", T), Err("the link is down".to_string()));
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}
