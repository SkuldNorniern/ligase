//! the agent panel: a coding agent (claude code, codex, opencode)
//! talked to over the agent client protocol. ligase starts the agent's acp
//! program and draws the conversation itself; the agent signs in on its
//! own and keeps its keys, ligase never sees them. the agent reads and
//! writes files through ligase, so open files show its edits at once and
//! each edit can be undone.

pub mod acp;
pub mod claude;
pub mod native;
pub mod view;

use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

use acp::{Fail, Incoming, Pipe};

/// the agents ligase knows how to start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    OpenCode,
    /// the built-in one, on any openai-compatible api.
    OpenAi,
}

impl Agent {
    pub const ALL: [Self; 4] = [Self::Claude, Self::Codex, Self::OpenCode, Self::OpenAi];

    /// the settings word.
    pub fn word(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::OpenAi => "custom",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        // `openai` was its name before profiles
        if word == "openai" {
            return Some(Self::OpenAi);
        }
        Self::ALL.into_iter().find(|a| a.word() == word)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::OpenAi => "Custom",
        }
    }

    /// the program each agent runs, all native: no node anywhere. claude
    /// code is its own `claude`, bridged in process from stream-json;
    /// codex speaks acp through the rust `codex-acp`; opencode has `acp`.
    fn launch(
        self,
    ) -> (
        &'static str,
        &'static [&'static str],
        &'static [&'static str],
    ) {
        // (program on PATH, its args, where to get it)
        match self {
            Self::Claude => ("claude", &[], &["https://claude.com/download"]),
            Self::Codex => (
                "codex-acp",
                &[],
                &["https://github.com/zed-industries/codex-acp/releases"],
            ),
            Self::OpenCode => ("opencode", &["acp"], &["https://opencode.ai"]),
            // runs inside ligase; never started as a program
            Self::OpenAi => ("", &[], &[]),
        }
    }

    /// the program and arguments that start it, or what is missing. reads
    /// the disk: worker only.
    pub fn command(self, custom: &str) -> Result<(PathBuf, Vec<String>), String> {
        let custom: Vec<String> = custom.split_whitespace().map(str::to_owned).collect();
        if let Some((program, args)) = custom.split_first() {
            let found =
                find_on_path(program).ok_or_else(|| format!("`{program}` is not on PATH"))?;
            return Ok(runnable(found, args.to_vec()));
        }
        let (program, args, from) = self.launch();
        if let Some(found) = find_on_path(program) {
            return Ok(runnable(
                found,
                args.iter().map(|a| (*a).to_owned()).collect(),
            ));
        }
        Err(match from.first() {
            Some(from) => format!("`{program}` is not on PATH; get it from {from}"),
            None => format!("`{program}` is not on PATH"),
        })
    }
}

/// a windows `.cmd` or `.bat` (npm installs those) runs through `cmd /d /c`.
fn runnable(found: PathBuf, args: Vec<String>) -> (PathBuf, Vec<String>) {
    let script = found
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    if !script {
        return (found, args);
    }
    let shell = std::env::var_os("COMSPEC").map_or_else(|| PathBuf::from("cmd.exe"), PathBuf::from);
    let mut all = vec![
        "/d".to_owned(),
        "/c".to_owned(),
        found.to_string_lossy().into_owned(),
    ];
    all.extend(args);
    (shell, all)
}

/// most PATH entries looked through.
const MAX_PATH_DIRS: usize = 256;

/// `program` in the PATH folders, with the endings windows runs.
pub fn find_on_path(program: &str) -> Option<PathBuf> {
    if program.contains(['/', '\\']) {
        let path = PathBuf::from(program);
        return path.is_file().then_some(path);
    }
    let path = std::env::var_os("PATH")?;
    let endings: &[&str] = if cfg!(windows) {
        &["exe", "cmd", "bat"]
    } else {
        &[""]
    };
    std::env::split_paths(&path)
        .take(MAX_PATH_DIRS)
        .find_map(|dir| find_in(&dir, program, endings))
}

fn find_in(dir: &Path, program: &str, endings: &[&str]) -> Option<PathBuf> {
    endings.iter().find_map(|ending| {
        let file = if ending.is_empty() {
            dir.join(program)
        } else {
            dir.join(format!("{program}.{ending}"))
        };
        file.is_file().then_some(file)
    })
}

/// most entries kept in a conversation; the oldest go first.
const MAX_ENTRIES: usize = 2000;
/// longest message text kept; streaming past it is cut.
const MAX_TEXT: usize = 512 * 1024;

/// one thing in the conversation.
#[derive(Debug)]
pub enum Entry {
    User {
        text: String,
        refs: Vec<Ref>,
    },
    Agent(String),
    /// the agent thinking out loud; drawn dim and short.
    Thought(String),
    Tool(Tool),
    /// the agent's plan: each step and how far it is.
    Plan(Vec<(String, Step)>),
    /// something ligase says: started, stopped, failed.
    Note {
        text: String,
        bad: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Pending,
    Doing,
    Done,
}

/// a tool the agent runs: read, edit, run a command.
#[derive(Debug, Default)]
pub struct Tool {
    pub id: String,
    pub title: String,
    /// acp's kind: read, edit, execute, search, ...
    pub kind: String,
    /// pending, in_progress, completed, failed.
    pub status: String,
    /// files it touches, with a line (from 1) when given.
    pub places: Vec<(PathBuf, Option<u32>)>,
    /// edits it makes: the file, lines gone, lines added.
    pub diffs: Vec<(PathBuf, usize, usize)>,
    /// what it said back, cut short.
    pub output: String,
}

/// a file, and lines in it, put before a message: sent as a link the
/// agent reads through ligase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ref {
    pub path: PathBuf,
    /// first and last line, from 1.
    pub lines: Option<(usize, usize)>,
}

impl Ref {
    /// `name:3-9`, for the chip.
    pub fn label(&self) -> String {
        let name = self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        match self.lines {
            Some((a, b)) if a == b => format!("{name}:{a}"),
            Some((a, b)) => format!("{name}:{a}-{b}"),
            None => name,
        }
    }

    fn uri(&self) -> String {
        let path = self.path.to_string_lossy().replace('\\', "/");
        let path = if path.starts_with('/') {
            path
        } else {
            format!("/{path}")
        };
        match self.lines {
            Some((a, b)) => format!("file://{path}#L{a}-{b}"),
            None => format!("file://{path}"),
        }
    }
}

/// where the conversation is.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Off,
    Starting,
    /// the agent wants a sign in first: its ways, (id, name, a terminal
    /// command when it signs in there).
    SignIn(Vec<SignIn>),
    Ready,
    /// a prompt is out; the agent is working.
    Working,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SignIn {
    pub id: String,
    pub name: String,
    /// for a sign in done in a terminal: the args to the agent's program.
    pub terminal: Option<Vec<String>>,
}

/// a permission the agent waits on.
#[derive(Debug)]
pub struct Permission {
    id: Value,
    pub title: String,
    /// (option id, label, allows)
    pub options: Vec<(String, String, bool)>,
}

/// what the panel wants from the rest of ligase.
#[derive(Debug, PartialEq)]
pub enum Need {
    /// the agent reads a file: answer from the open buffer, else the disk.
    Read {
        id: Value,
        path: PathBuf,
        line: Option<usize>,
        limit: Option<usize>,
    },
    /// the agent writes a file: into the open buffer as an edit, else to disk.
    Write {
        id: Value,
        path: PathBuf,
        text: String,
        /// the file before, when the agent changed it on disk itself.
        before: Option<String>,
    },
    /// sign in by running the agent's program with these args in a terminal.
    Terminal(Vec<String>),
}

/// what was asked and is waiting for its reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Init,
    Session,
    SignIn,
    Prompt,
    Mode,
    Model,
}

/// one of the agent's modes or models: its id, name and a line on it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    pub name: String,
    pub about: String,
}

/// acp's list of modes or models: the ids under `id`, at most 16.
fn choices(list: &Value, id: &str) -> Vec<Choice> {
    list.as_array()
        .into_iter()
        .flatten()
        .take(16)
        .filter_map(|m| {
            let id: String = m[id].as_str()?.chars().take(60).collect();
            let name = m["name"]
                .as_str()
                .map_or_else(|| id.clone(), |n| n.chars().take(40).collect());
            let about = m["description"]
                .as_str()
                .map(|d| d.chars().take(120).collect())
                .unwrap_or_default();
            Some(Choice { id, name, about })
        })
        .collect()
}

/// a saved conversation, listed in the history.
#[derive(Clone, Debug, PartialEq)]
pub struct Past {
    pub path: PathBuf,
    /// its first message, cut short.
    pub title: String,
    /// who it was with.
    pub agent: String,
    /// seconds since 1970.
    pub when: u64,
}

/// what the panel shows over the conversation, when not the conversation.
#[derive(Debug, Default)]
pub enum Over {
    #[default]
    None,
    /// the saved conversations, newest first.
    History(Vec<Past>),
    /// one saved conversation, read only: its title, entries, their laid
    /// out rows and the scroll.
    Past(String, Vec<Entry>, view::Cache, f32),
    /// agents to switch to: (label, settings word, model profile).
    Agents(Vec<(String, String, Option<String>)>),
    /// the modes to pick from, over the box.
    Modes,
    /// the models to pick from, over the box.
    Models,
}

/// the built-in agent's setup card, filled in by the person.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Setup {
    /// the profile's name, as typed.
    pub name: String,
    pub base: String,
    pub model: String,
    /// the context in tokens, as typed.
    pub context: String,
    /// typed here only; never shown back. empty keeps the key there is.
    pub key: String,
    /// the profile being edited, by its saved name; none for a new one.
    pub editing: Option<String>,
    /// every saved profile's name, for the chips.
    pub profiles: Vec<String>,
    /// profiles with a key in the keys file.
    pub saved_keys: Vec<String>,
}

/// one agent conversation and its pipe.
pub struct Chat {
    pub agent: Agent,
    /// shown in the header: the agent, or the built-in one's model.
    pub title: String,
    /// the setup card, while it is open.
    pub setup: Option<Setup>,
    pub state: State,
    pub entries: Vec<Entry>,
    pub input: String,
    pub refs: Vec<Ref>,
    pub permission: Option<Permission>,
    pub scroll: f32,
    /// stay at the bottom as the agent writes, until scrolled up.
    pub follow: bool,
    /// the workspace: the agent works here and only here.
    root: PathBuf,
    pipe: Option<Pipe>,
    /// a pipe let go, waiting to be dropped off the ui thread.
    dead: Option<Pipe>,
    session: Option<String>,
    sign_in: Vec<SignIn>,
    pending: Vec<(u64, Pending)>,
    /// the prompt to send once the session is up.
    queued: Option<Value>,
    /// stop was asked once this turn; asked again, the agent is ended.
    stopping: bool,
    /// the agent's modes and the one it is in.
    pub modes: Vec<Choice>,
    pub mode: Option<String>,
    /// the models the agent offers and the one in use.
    pub models: Vec<Choice>,
    pub model: Option<String>,
    /// history, a past conversation or the agent picker over the talk.
    pub over: Over,
    /// laid out rows per entry, kept while width and text stay.
    pub(crate) laid: view::Cache,
    /// how far the picker or history list is scrolled.
    pub(crate) list_scroll: f32,
}

impl Chat {
    pub fn new(agent: Agent, root: PathBuf) -> Self {
        Self {
            agent,
            title: agent.name().to_owned(),
            setup: None,
            state: State::Off,
            entries: Vec::new(),
            input: String::new(),
            refs: Vec::new(),
            permission: None,
            scroll: 0.0,
            follow: true,
            root,
            pipe: None,
            dead: None,
            session: None,
            sign_in: Vec::new(),
            pending: Vec::new(),
            queued: None,
            stopping: false,
            modes: Vec::new(),
            mode: None,
            models: Vec::new(),
            model: None,
            over: Over::None,
            laid: view::Cache::default(),
            list_scroll: 0.0,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// the start job came back.
    pub fn started(&mut self, pipe: Result<Pipe, String>) {
        match pipe {
            Ok(mut pipe) => {
                let id = pipe.request(
                    "initialize",
                    json!({
                        "protocolVersion": 1,
                        "clientCapabilities": {
                            "fs": {"readTextFile": true, "writeTextFile": true},
                            "terminal": false,
                        },
                        "clientInfo": {"name": "ligase", "title": "Ligase", "version": env!("CARGO_PKG_VERSION")},
                    }),
                );
                self.pending.push((id, Pending::Init));
                self.pipe = Some(pipe);
            }
            Err(why) => self.fail(why),
        }
    }

    fn fail(&mut self, why: String) {
        self.note(why.clone(), true);
        self.state = State::Failed(why);
        if let Some(pipe) = self.pipe.take() {
            self.dead = Some(pipe);
        }
        self.session = None;
        self.pending.clear();
        self.permission = None;
        // a message waiting on the session does not go out on a later start
        // by itself, and a stop asked for has nothing left to stop
        self.queued = None;
        self.stopping = false;
    }

    fn note(&mut self, text: String, bad: bool) {
        self.push(Entry::Note { text, bad });
    }

    fn push(&mut self, entry: Entry) {
        if self.entries.len() >= MAX_ENTRIES {
            self.entries.remove(0);
            self.laid.clear();
        }
        self.entries.push(entry);
    }

    /// sends what is typed, with its refs. starts the session first when
    /// it isn't up; false when there is nothing to send or the agent is busy.
    pub fn send(&mut self) -> bool {
        let text = self.input.trim().to_owned();
        // one message at a time: one out, or one waiting for the session
        if text.is_empty() || self.state == State::Working || self.queued.is_some() {
            return false;
        }
        let refs = std::mem::take(&mut self.refs);
        let mut prompt = vec![json!({"type": "text", "text": text})];
        for r in &refs {
            prompt.push(json!({
                "type": "resource_link",
                "uri": r.uri(),
                "name": r.label(),
            }));
        }
        self.input.clear();
        self.push(Entry::User { text, refs });
        self.follow = true;
        let prompt = Value::Array(prompt);
        match (&mut self.pipe, &self.session) {
            (Some(pipe), Some(session)) => {
                let id = pipe.request(
                    "session/prompt",
                    json!({"sessionId": session, "prompt": prompt}),
                );
                self.pending.push((id, Pending::Prompt));
                self.state = State::Working;
                self.stopping = false;
            }
            _ => self.queued = Some(prompt),
        }
        true
    }

    /// stops the agent's turn: it is told to cancel, a waiting permission
    /// is answered cancelled. a second stop in the same turn ends an agent
    /// that didn't listen: its pipe comes back, to be dropped off the ui
    /// thread.
    pub fn stop(&mut self) -> Option<Pipe> {
        if self.stopping {
            self.fail(format!("{} was ended", self.agent.name()));
            return self.dead.take();
        }
        self.stopping = true;
        self.stop_permission();
        if let (Some(pipe), Some(session)) = (&self.pipe, &self.session) {
            pipe.notify("session/cancel", json!({"sessionId": session}));
        }
        None
    }

    /// a pipe let go, taken out to be dropped off the ui thread: its
    /// close waits on the program.
    pub fn take_dead(&mut self) -> Option<Pipe> {
        self.dead.take()
    }

    /// the person picked a permission option.
    pub fn answer(&mut self, option: &str) {
        if let Some(permission) = self.permission.take()
            && let Some(pipe) = &self.pipe
        {
            pipe.reply(
                &permission.id,
                Ok(json!({"outcome": {"outcome": "selected", "optionId": option}})),
            );
        }
    }

    /// signs in the way `n`: the agent's own, or a terminal one ligase
    /// runs for it.
    pub fn sign_in(&mut self, n: usize) -> Option<Need> {
        let way = self.sign_in.get(n)?.clone();
        if let Some(args) = way.terminal {
            return Some(Need::Terminal(args));
        }
        let pipe = self.pipe.as_mut()?;
        let id = pipe.request("authenticate", json!({"methodId": way.id}));
        self.pending.push((id, Pending::SignIn));
        self.state = State::Starting;
        None
    }

    /// after a sign in elsewhere: ask for the session again.
    pub fn retry(&mut self) {
        self.new_session();
    }

    fn new_session(&mut self) {
        let Some(pipe) = &mut self.pipe else { return };
        let id = pipe.request(
            "session/new",
            json!({"cwd": self.root.to_string_lossy(), "mcpServers": []}),
        );
        self.pending.push((id, Pending::Session));
        self.state = State::Starting;
    }

    /// answers a read or write ligase did for the agent.
    pub fn respond(&self, id: &Value, result: Result<Value, String>) {
        if let Some(pipe) = &self.pipe {
            pipe.reply(
                id,
                result.map_err(|message| Fail {
                    code: -32603,
                    message,
                }),
            );
        }
    }

    /// takes in what the agent sent; gives what ligase must do for it.
    pub fn poll(&mut self) -> Vec<Need> {
        let mut needs = Vec::new();
        let mut came = Vec::new();
        if let Some(pipe) = &self.pipe {
            while let Ok(message) = pipe.from_agent.try_recv() {
                came.push(message);
            }
        }
        for message in came {
            match message {
                Incoming::Reply(id, result) => self.replied(id, result),
                Incoming::Request(id, method, params) => {
                    if let Some(need) = self.asked(id, &method, &params) {
                        needs.push(need);
                    }
                }
                Incoming::Note(method, params) => {
                    // another session's news is not this conversation's
                    let ours = self.session.as_deref() == params["sessionId"].as_str();
                    if method == "session/update" && ours {
                        self.update(&params["update"]);
                    }
                }
                Incoming::Gone(said) => {
                    let why = if said.trim().is_empty() {
                        format!("{} stopped", self.agent.name())
                    } else {
                        format!("{} stopped:\n{said}", self.agent.name())
                    };
                    self.fail(why);
                    break;
                }
            }
        }
        needs
    }

    fn replied(&mut self, id: u64, result: Result<Value, Fail>) {
        let Some(at) = self.pending.iter().position(|(p, _)| *p == id) else {
            return;
        };
        let (_, what) = self.pending.remove(at);
        match (what, result) {
            (Pending::Init, Ok(result)) => {
                self.sign_in = sign_ins(&result["authMethods"]);
                self.new_session();
            }
            (Pending::Session, Ok(result)) => match result["sessionId"].as_str() {
                Some(session) => {
                    self.session = Some(session.to_owned());
                    self.state = State::Ready;
                    self.take_modes(&result["modes"]);
                    self.take_models(&result["models"]);
                    if let Some(prompt) = self.queued.take()
                        && let Some(pipe) = &mut self.pipe
                    {
                        let id = pipe.request(
                            "session/prompt",
                            json!({"sessionId": session, "prompt": prompt}),
                        );
                        self.pending.push((id, Pending::Prompt));
                        self.state = State::Working;
                    }
                }
                None => self.fail("the agent gave no session".to_owned()),
            },
            // acp's "authentication required" and its kin: sign in first
            (Pending::Session, Err(fail))
                if fail.code == -32000 || fail.message.to_lowercase().contains("auth") =>
            {
                if self.sign_in.is_empty() {
                    self.fail(fail.message);
                } else {
                    self.state = State::SignIn(self.sign_in.clone());
                }
            }
            (Pending::SignIn, Ok(_)) => self.new_session(),
            (Pending::Mode, Ok(_)) => {}
            (Pending::Mode | Pending::Model, Err(fail)) => self.note(fail.message, true),
            (Pending::Model, Ok(_)) => {}
            (Pending::Prompt, Ok(result)) => {
                self.state = State::Ready;
                self.stopping = false;
                let stop = result["stopReason"].as_str().unwrap_or("end_turn");
                let why = match stop {
                    "max_tokens" => Some("the answer hit its length limit"),
                    "max_turn_requests" => Some("the agent hit its step limit"),
                    "refusal" => Some("the agent refused"),
                    "cancelled" => Some("stopped"),
                    _ => None,
                };
                if let Some(why) = why {
                    self.note(why.to_owned(), stop == "refusal");
                }
                // a card left from the turn still gets its answer
                self.stop_permission();
            }
            (Pending::Prompt, Err(fail)) => {
                self.state = State::Ready;
                self.stopping = false;
                self.stop_permission();
                self.note(fail.message, true);
            }
            (Pending::Init | Pending::Session | Pending::SignIn, Err(fail)) => {
                self.fail(fail.message)
            }
        }
    }

    /// a request from the agent. fs ones go to ligase; paths outside the
    /// workspace are refused here.
    fn asked(&mut self, id: Value, method: &str, params: &Value) -> Option<Need> {
        let refuse = |this: &Self, why: &str| {
            if let Some(pipe) = &this.pipe {
                pipe.reply(
                    &id,
                    Err(Fail {
                        code: -32602,
                        message: why.to_owned(),
                    }),
                );
            }
        };
        match method {
            "fs/read_text_file" | "fs/write_text_file" => {
                let Some(path) = params["path"]
                    .as_str()
                    .and_then(|p| inside(&self.root, Path::new(p)))
                else {
                    refuse(self, "the path is outside the workspace");
                    return None;
                };
                if method == "fs/read_text_file" {
                    let num =
                        |key: &str| params[key].as_u64().and_then(|n| usize::try_from(n).ok());
                    return Some(Need::Read {
                        id,
                        path,
                        line: num("line"),
                        limit: num("limit"),
                    });
                }
                let Some(text) = params["content"].as_str() else {
                    refuse(self, "no content");
                    return None;
                };
                let before = params["_meta"]["before"]
                    .as_str()
                    .filter(|b| b.len() as u64 <= crate::document::MAX_FILE)
                    .map(str::to_owned);
                Some(Need::Write {
                    id,
                    path,
                    text: text.to_owned(),
                    before,
                })
            }
            // after stop, nothing more is allowed this turn
            "session/request_permission" if self.stopping => {
                if let Some(pipe) = &self.pipe {
                    pipe.reply(&id, Ok(json!({"outcome": {"outcome": "cancelled"}})));
                }
                None
            }
            "session/request_permission" => {
                let call = &params["toolCall"];
                let title = call["title"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| self.tool_title(call["toolCallId"].as_str()?))
                    .unwrap_or_else(|| "the agent asks to go on".to_owned());
                let options: Vec<(String, String, bool)> = params["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(8)
                    .filter_map(|o| {
                        let kind = o["kind"].as_str().unwrap_or("");
                        Some((
                            o["optionId"].as_str()?.to_owned(),
                            o["name"]
                                .as_str()
                                .unwrap_or(kind)
                                .chars()
                                .take(60)
                                .collect(),
                            kind.starts_with("allow"),
                        ))
                    })
                    .collect();
                if options.is_empty() {
                    refuse(self, "no options");
                    return None;
                }
                // an older one still waiting can't be answered any more
                self.stop_permission();
                self.permission = Some(Permission {
                    id,
                    title: title.chars().take(300).collect(),
                    options,
                });
                self.follow = true;
                None
            }
            _ => {
                if let Some(pipe) = &self.pipe {
                    pipe.reply(
                        &id,
                        Err(Fail {
                            code: -32601,
                            message: format!("{method} is not supported"),
                        }),
                    );
                }
                None
            }
        }
    }

    fn stop_permission(&mut self) {
        if let Some(old) = self.permission.take()
            && let Some(pipe) = &self.pipe
        {
            pipe.reply(&old.id, Ok(json!({"outcome": {"outcome": "cancelled"}})));
        }
    }

    fn tool_title(&self, id: &str) -> Option<String> {
        self.entries.iter().rev().find_map(|e| match e {
            Entry::Tool(t) if t.id == id => Some(t.title.clone()),
            _ => None,
        })
    }

    /// a `session/update`: text streams onto the last message of its kind,
    /// tool calls are made and changed by id, the plan is replaced whole.
    fn update(&mut self, update: &Value) {
        let kind = update["sessionUpdate"].as_str().unwrap_or("");
        match kind {
            "agent_message_chunk" | "agent_thought_chunk" => {
                let Some(text) = text_of(&update["content"]) else {
                    return;
                };
                let thought = kind == "agent_thought_chunk";
                match self.entries.last_mut() {
                    Some(Entry::Agent(t)) if !thought => grow(t, &text),
                    Some(Entry::Thought(t)) if thought => grow(t, &text),
                    _ => self.push(if thought {
                        Entry::Thought(text)
                    } else {
                        Entry::Agent(text)
                    }),
                }
            }
            "tool_call" | "tool_call_update" => {
                let Some(id) = update["toolCallId"].as_str() else {
                    return;
                };
                let at = self
                    .entries
                    .iter()
                    .rposition(|e| matches!(e, Entry::Tool(t) if t.id == id));
                let tool = match at {
                    Some(i) => match &mut self.entries[i] {
                        Entry::Tool(t) => t,
                        _ => return,
                    },
                    None => {
                        self.push(Entry::Tool(Tool {
                            id: id.to_owned(),
                            status: "pending".to_owned(),
                            ..Tool::default()
                        }));
                        match self.entries.last_mut() {
                            Some(Entry::Tool(t)) => t,
                            _ => return,
                        }
                    }
                };
                tool_fields(tool, update);
                // laid out rows of a changed tool are stale
                let i = at.unwrap_or(self.entries.len() - 1);
                self.laid.forget(i);
            }
            "current_mode_update" => {
                if let Some(id) = update["currentModeId"].as_str() {
                    self.mode = Some(id.chars().take(60).collect());
                }
            }
            "plan" => {
                let steps: Vec<(String, Step)> = update["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(100)
                    .map(|e| {
                        let step = match e["status"].as_str() {
                            Some("completed") => Step::Done,
                            Some("in_progress") => Step::Doing,
                            _ => Step::Pending,
                        };
                        (
                            e["content"]
                                .as_str()
                                .unwrap_or("")
                                .chars()
                                .take(300)
                                .collect(),
                            step,
                        )
                    })
                    .collect();
                match self
                    .entries
                    .iter()
                    .rposition(|e| matches!(e, Entry::Plan(_)))
                {
                    // the same turn's plan changes in place
                    Some(i)
                        if !self.entries[i..]
                            .iter()
                            .any(|e| matches!(e, Entry::User { .. })) =>
                    {
                        self.entries[i] = Entry::Plan(steps);
                        self.laid.forget(i);
                    }
                    _ => self.push(Entry::Plan(steps)),
                }
            }
            _ => {}
        }
    }

    /// acp's `modes`: what the agent offers and where it is.
    fn take_modes(&mut self, modes: &Value) {
        self.modes = choices(&modes["availableModes"], "id");
        self.mode = modes["currentModeId"]
            .as_str()
            .map(|m| m.chars().take(60).collect());
    }

    /// acp's `models`: what the agent can run on and which one it is.
    fn take_models(&mut self, models: &Value) {
        self.models = choices(&models["availableModels"], "modelId");
        self.model = models["currentModelId"]
            .as_str()
            .map(|m| m.chars().take(60).collect());
    }

    /// the mode it is in, or the first.
    pub fn current_mode(&self) -> Option<&Choice> {
        let at = self.mode.as_ref();
        self.modes
            .iter()
            .find(|c| Some(&c.id) == at)
            .or(self.modes.first())
    }

    /// the model in use, or the first.
    pub fn current_model(&self) -> Option<&Choice> {
        let at = self.model.as_ref();
        self.models
            .iter()
            .find(|c| Some(&c.id) == at)
            .or(self.models.first())
    }

    /// moves the agent to model `id`; shown at once, the agent says if not.
    pub fn set_model(&mut self, id: &str) {
        let (Some(pipe), Some(session)) = (&mut self.pipe, &self.session) else {
            return;
        };
        let rid = pipe.request(
            "session/set_model",
            json!({"sessionId": session, "modelId": id}),
        );
        self.pending.push((rid, Pending::Model));
        self.model = Some(id.to_owned());
    }

    /// moves the agent to mode `id`; shown at once, the agent says if not.
    pub fn set_mode(&mut self, id: &str) {
        let (Some(pipe), Some(session)) = (&mut self.pipe, &self.session) else {
            return;
        };
        let rid = pipe.request(
            "session/set_mode",
            json!({"sessionId": session, "modeId": id}),
        );
        self.pending.push((rid, Pending::Mode));
        self.mode = Some(id.to_owned());
    }

    /// the mode after the one it is in, round; for shift+tab.
    pub fn next_mode(&self) -> Option<String> {
        let at = self
            .mode
            .as_ref()
            .and_then(|m| self.modes.iter().position(|c| &c.id == m))
            .map_or(0, |i| i + 1);
        self.modes
            .get(at % self.modes.len().max(1))
            .map(|c| c.id.clone())
    }

    /// the conversation as a plain record to keep: none when nothing was
    /// said. text only; tool steps by their title.
    pub fn record(&self) -> Option<Value> {
        let first = self.entries.iter().find_map(|e| match e {
            Entry::User { text, .. } => Some(text.clone()),
            _ => None,
        })?;
        let entries: Vec<Value> = self
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::User { text, .. } => Some(json!({"kind": "user", "text": text})),
                Entry::Agent(t) => Some(json!({"kind": "agent", "text": t})),
                Entry::Tool(t) => Some(json!({"kind": "tool", "text": t.title, "status": t.status})),
                Entry::Plan(steps) => Some(json!({"kind": "plan", "text": steps.iter().map(|s| s.0.clone()).collect::<Vec<_>>().join("\n")})),
                Entry::Note { text, bad } => Some(json!({"kind": "note", "text": text, "bad": bad})),
                Entry::Thought(_) => None,
            })
            .collect();
        let when = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Some(json!({
            "title": first.chars().take(120).collect::<String>(),
            "agent": self.title,
            "when": when,
            "entries": entries,
        }))
    }

    /// whether the pipe is up or on its way.
    pub fn running(&self) -> bool {
        self.pipe.is_some() || self.state == State::Starting
    }
}

/// most conversations kept; the oldest go.
const MAX_PAST: usize = 200;

/// the folder saved conversations live in, beside the settings.
fn history_dir() -> Option<PathBuf> {
    aurea::config_dir("ligase").map(|d| d.join("agent-history"))
}

/// keeps `record` as a file, the oldest past the cap let go. worker only.
pub fn keep(record: &Value) -> Result<(), String> {
    let dir = history_dir().ok_or("no config folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let when = record["when"].as_u64().unwrap_or(0);
    let mut n = 0;
    let path = loop {
        let p = dir.join(format!("{when}-{n}.json"));
        if !p.exists() {
            break p;
        }
        n += 1;
    };
    std::fs::write(&path, record.to_string()).map_err(|e| e.to_string())?;
    let mut all: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    if all.len() > MAX_PAST {
        all.sort();
        for old in &all[..all.len() - MAX_PAST] {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(())
}

/// the saved conversations, newest first. worker only.
pub fn history() -> Vec<Past> {
    let Some(dir) = history_dir() else {
        return Vec::new();
    };
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Past> = read
        .flatten()
        .take(MAX_PAST * 2)
        .filter_map(|e| {
            let path = e.path();
            if e.metadata().ok()?.len() > 16 * 1024 * 1024 {
                return None;
            }
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
            Some(Past {
                title: v["title"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(120)
                    .collect(),
                agent: v["agent"].as_str().unwrap_or("").chars().take(60).collect(),
                when: v["when"].as_u64().unwrap_or(0),
                path,
            })
        })
        .collect();
    out.sort_by_key(|p| std::cmp::Reverse(p.when));
    out
}

/// a saved conversation's entries, to read. worker only.
pub fn past(path: &Path) -> Result<Vec<Entry>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if text.len() > 16 * 1024 * 1024 {
        return Err("too big".to_owned());
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(v["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_ENTRIES)
        .filter_map(|e| {
            let text: String = e["text"].as_str()?.chars().take(MAX_TEXT).collect();
            Some(match e["kind"].as_str()? {
                "user" => Entry::User {
                    text,
                    refs: Vec::new(),
                },
                "agent" => Entry::Agent(text),
                "tool" => Entry::Tool(Tool {
                    title: text,
                    status: e["status"].as_str().unwrap_or("completed").to_owned(),
                    ..Tool::default()
                }),
                "plan" => Entry::Plan(text.lines().map(|l| (l.to_owned(), Step::Done)).collect()),
                _ => Entry::Note {
                    text,
                    bad: e["bad"].as_bool().unwrap_or(false),
                },
            })
        })
        .collect())
}

/// adds `more` to a streamed message, up to the cap.
fn grow(text: &mut String, more: &str) {
    if text.len() + more.len() <= MAX_TEXT {
        text.push_str(more);
    }
}

/// the text of a content block; other kinds say what they are.
fn text_of(content: &Value) -> Option<String> {
    match content["type"].as_str()? {
        "text" => content["text"].as_str().map(str::to_owned),
        "resource_link" => content["name"].as_str().map(|n| format!("[{n}]")),
        "image" => Some("[image]".to_owned()),
        _ => None,
    }
}

fn tool_fields(tool: &mut Tool, update: &Value) {
    let short = |v: &Value, n: usize| v.as_str().map(|s| s.chars().take(n).collect::<String>());
    if let Some(title) = short(&update["title"], 300) {
        tool.title = title;
    }
    if let Some(kind) = short(&update["kind"], 20) {
        tool.kind = kind;
    }
    if let Some(status) = short(&update["status"], 20) {
        tool.status = status;
    }
    if let Some(places) = update["locations"].as_array() {
        tool.places = places
            .iter()
            .take(20)
            .filter_map(|l| {
                let path = PathBuf::from(l["path"].as_str()?);
                let line = l["line"].as_u64().and_then(|n| u32::try_from(n).ok());
                Some((path, line))
            })
            .collect();
    }
    if let Some(content) = update["content"].as_array() {
        tool.diffs.clear();
        tool.output.clear();
        for part in content.iter().take(50) {
            match part["type"].as_str() {
                Some("diff") => {
                    let old = part["oldText"].as_str().unwrap_or("");
                    let new = part["newText"].as_str().unwrap_or("");
                    if let Some(path) = part["path"].as_str() {
                        let (gone, added) = line_change(old, new);
                        tool.diffs.push((PathBuf::from(path), gone, added));
                    }
                }
                Some("content") => {
                    if let Some(text) = text_of(&part["content"])
                        && tool.output.len() < 4000
                    {
                        tool.output
                            .push_str(&text.chars().take(4000).collect::<String>());
                    }
                }
                _ => {}
            }
        }
    }
}

/// lines gone and added between two texts, counted after the lines both
/// share at the start and end.
fn line_change(old: &str, new: &str) -> (usize, usize) {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let front = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let back = a[front..]
        .iter()
        .rev()
        .zip(b[front..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    (a.len() - front - back, b.len() - front - back)
}

fn sign_ins(methods: &Value) -> Vec<SignIn> {
    methods
        .as_array()
        .into_iter()
        .flatten()
        .take(8)
        .filter_map(|m| {
            let terminal = (m["type"].as_str() == Some("terminal")).then(|| {
                m["args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .take(16)
                    .map(str::to_owned)
                    .collect()
            });
            Some(SignIn {
                id: m["id"].as_str()?.to_owned(),
                name: m["name"]
                    .as_str()
                    .unwrap_or("Sign in")
                    .chars()
                    .take(60)
                    .collect(),
                terminal,
            })
        })
        .collect()
}

/// `path` when it is inside `root`, with no `..` to climb out. the agent
/// gives absolute paths; anything else is refused.
pub fn inside(root: &Path, path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    // windows: the drive letter's case may differ between the two
    let low = |p: &Path| p.to_string_lossy().replace('\\', "/").to_lowercase();
    let (r, p) = (low(root), low(path));
    let r = r.trim_end_matches('/');
    let fits = if cfg!(windows) {
        p == r || p.starts_with(&format!("{r}/"))
    } else {
        path.starts_with(root)
    };
    fits.then(|| path.to_path_buf())
}

/// the lines `line` (from 1) on, `limit` of them, as acp's read asks.
pub fn lines_of(text: &str, line: Option<usize>, limit: Option<usize>) -> String {
    if line.is_none() && limit.is_none() {
        return text.to_owned();
    }
    let skip = line.unwrap_or(1).saturating_sub(1);
    let take = limit.unwrap_or(usize::MAX);
    let mut out = String::new();
    for l in text.split_inclusive('\n').skip(skip).take(take) {
        out.push_str(l);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_start_forgets_the_waiting_message() {
        let mut c = chat();
        c.state = State::Starting;
        c.input = "fix it".to_owned();
        c.send();
        assert!(c.queued.is_some());
        c.started(Err("no program".to_owned()));
        assert!(c.queued.is_none() && !c.stopping);
        c.input = "again".to_owned();
        assert!(c.send(), "send works after a failed start");
    }

    #[test]
    fn a_kept_conversation_reads_back() {
        let mut c = chat();
        assert!(c.record().is_none(), "nothing said, nothing kept");
        c.entries.push(Entry::User {
            text: "fix the bug".to_owned(),
            refs: Vec::new(),
        });
        c.entries.push(Entry::Thought("hm".to_owned()));
        c.entries.push(Entry::Agent("done".to_owned()));
        let record = c.record().expect("a record");
        assert_eq!(record["title"], "fix the bug");
        let dir = std::env::temp_dir().join(format!("ligase-past-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("one.json");
        std::fs::write(&file, record.to_string()).expect("write");
        let back = past(&file).expect("read");
        assert_eq!(back.len(), 2, "thoughts are not kept");
        assert!(matches!(&back[0], Entry::User { text, .. } if text == "fix the bug"));
        assert!(matches!(&back[1], Entry::Agent(t) if t == "done"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_history_files_do_not_panic() {
        let dir = std::env::temp_dir().join(format!("ligase-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let cases = [
            "",
            "{",
            "[]",
            r#"{"entries": 5}"#,
            r#"{"entries": [1, null, {"kind": 3}, {"text": "x"}, {"kind": "tool", "text": "t", "status": 9}]}"#,
        ];
        for (i, text) in cases.iter().enumerate() {
            let file = dir.join(format!("{i}.json"));
            std::fs::write(&file, text).expect("write");
            if let Ok(entries) = past(&file) {
                assert!(entries.len() <= 1, "{text}");
            }
        }
        assert!(past(&dir.join("missing.json")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn chat() -> Chat {
        Chat::new(
            Agent::Claude,
            PathBuf::from(if cfg!(windows) { "C:\\w" } else { "/w" }),
        )
    }

    #[test]
    fn paths_stay_in_the_workspace() {
        let root = Path::new(if cfg!(windows) { "C:\\w" } else { "/w" });
        let ok = if cfg!(windows) {
            "c:\\w\\src\\a.rs"
        } else {
            "/w/src/a.rs"
        };
        assert!(inside(root, Path::new(ok)).is_some());
        let up = if cfg!(windows) {
            "C:\\w\\..\\x"
        } else {
            "/w/../x"
        };
        assert!(inside(root, Path::new(up)).is_none());
        let next = if cfg!(windows) { "C:\\wx\\a" } else { "/wx/a" };
        assert!(
            inside(root, Path::new(next)).is_none(),
            "a sibling with the same start"
        );
        assert!(inside(root, Path::new("src/a.rs")).is_none(), "relative");
    }

    #[test]
    fn reads_take_lines_from_one() {
        let text = "a\nb\nc\nd";
        assert_eq!(lines_of(text, None, None), text);
        assert_eq!(lines_of(text, Some(2), Some(2)), "b\nc\n");
        assert_eq!(lines_of(text, Some(4), None), "d");
        assert_eq!(lines_of(text, Some(9), None), "");
    }

    #[test]
    fn modes_and_models_come_with_the_session() {
        let mut c = chat();
        c.take_modes(&json!({"currentModeId": "plan", "availableModes": [
            {"id": "default", "name": "Ask first", "description": "asks"},
            {"id": "plan", "name": "Plan only"},
            {"name": "no id"}
        ]}));
        c.take_models(&json!({"currentModelId": "gone", "availableModels": [
            {"modelId": "opus", "name": "Opus", "description": "big"},
            {"modelId": "haiku"}
        ]}));
        assert_eq!(c.modes.len(), 2);
        assert_eq!(c.current_mode().map(|m| m.name.as_str()), Some("Plan only"));
        assert_eq!(c.modes[0].about, "asks");
        // the last mode wraps round to the first
        assert_eq!(c.next_mode().as_deref(), Some("default"));
        // a current model not listed falls back to the first
        assert_eq!(c.current_model().map(|m| m.id.as_str()), Some("opus"));
        assert_eq!(c.models[1].name, "haiku");
    }

    #[test]
    fn updates_stream_into_entries() {
        let mut c = chat();
        let chunk = |t: &str| json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": t}});
        c.update(&chunk("Hel"));
        c.update(&chunk("lo"));
        c.update(&json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "hm"}}));
        c.update(&chunk("next"));
        assert!(matches!(&c.entries[0], Entry::Agent(t) if t == "Hello"));
        assert!(matches!(&c.entries[1], Entry::Thought(t) if t == "hm"));
        assert!(matches!(&c.entries[2], Entry::Agent(t) if t == "next"));
        c.update(&json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read a.rs", "kind": "read", "status": "pending"}));
        c.update(&json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
            "content": [{"type": "diff", "path": "/w/a.rs", "oldText": "a\nb\nc", "newText": "a\nB\nc\nd"}]}));
        let Entry::Tool(t) = &c.entries[3] else {
            panic!()
        };
        assert_eq!(
            (t.title.as_str(), t.status.as_str()),
            ("Read a.rs", "completed")
        );
        // the new last line leaves no shared tail: b, c out; B, c, d in
        assert_eq!(t.diffs[0].1..t.diffs[0].2, 2..3);
        c.update(&json!({"sessionUpdate": "plan", "entries": [{"content": "x", "priority": "high", "status": "pending"}]}));
        c.update(&json!({"sessionUpdate": "plan", "entries": [{"content": "x", "priority": "high", "status": "completed"}]}));
        assert_eq!(
            c.entries
                .iter()
                .filter(|e| matches!(e, Entry::Plan(_)))
                .count(),
            1,
            "replaced"
        );
        assert!(matches!(&c.entries[4], Entry::Plan(s) if s[0].1 == Step::Done));
        c.update(&json!({"sessionUpdate": "nonsense"}));
        c.update(&json!(null));
        assert_eq!(c.entries.len(), 5);
    }

    #[test]
    fn a_session_waits_for_sign_in() {
        let mut c = chat();
        c.pending.push((1, Pending::Init));
        c.replied(1, Ok(json!({"authMethods": [{"id": "login", "name": "Log in", "type": "terminal", "args": ["/login"]}]})));
        // no pipe: no session asked; set as if asked
        c.pending.push((2, Pending::Session));
        c.replied(
            2,
            Err(Fail {
                code: -32000,
                message: "Authentication required".into(),
            }),
        );
        let State::SignIn(ways) = &c.state else {
            panic!("{:?}", c.state)
        };
        assert_eq!(
            ways[0].terminal.as_deref(),
            Some(&["/login".to_owned()][..])
        );
        assert_eq!(
            c.sign_in(0),
            Some(Need::Terminal(vec!["/login".to_owned()]))
        );
    }

    #[test]
    fn a_message_before_the_session_waits() {
        let mut c = chat();
        c.input = "  fix it ".into();
        c.refs.push(Ref {
            path: PathBuf::from("/w/a.rs"),
            lines: Some((3, 9)),
        });
        assert!(c.send());
        assert!(c.input.is_empty() && c.refs.is_empty());
        let queued = c.queued.as_ref().unwrap();
        assert_eq!(queued[0]["text"], "fix it");
        assert_eq!(queued[1]["uri"], "file:///w/a.rs#L3-9");
        assert_eq!(queued[1]["name"], "a.rs:3-9");
        c.input = "  ".into();
        assert!(!c.send(), "nothing typed");
    }

    #[test]
    fn a_second_message_never_replaces_a_waiting_one() {
        let mut c = chat();
        c.input = "first".into();
        assert!(c.send());
        c.input = "second".into();
        assert!(!c.send(), "the first still waits for the session");
        assert_eq!(c.input, "second", "kept to send later");
        assert_eq!(c.queued.as_ref().unwrap()[0]["text"], "first");
    }

    #[test]
    fn changes_count_lines() {
        assert_eq!(line_change("a\nb\nc", "a\nb\nc"), (0, 0));
        assert_eq!(line_change("a\nb\nc", "a\nx\ny\nc"), (1, 2));
        assert_eq!(line_change("", "new\nfile"), (0, 2));
    }
}
