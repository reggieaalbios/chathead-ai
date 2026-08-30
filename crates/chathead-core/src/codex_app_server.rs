//! Minimal typed client for the experimental Codex app-server JSONL protocol.

use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use crate::{
    AgentActivity, AgentActivityKind, AgentQuestion, AgentQuestionField, AgentQuestionOption,
    AgentRequestId, ChatMode, ChatPrompt,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::mpsc,
    task::JoinHandle,
    time::timeout,
};

const CODEX_BINARY_ENV: &str = "CHATHEAD_CODEX_BIN";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const CHANNEL_CAPACITY: usize = 64;
const DEVELOPER_INSTRUCTIONS: &str = "You are a conversational assistant inside ChatHead. Answer the user's questions directly. You may understand text and images supplied in the user's prompt. Do not inspect files other than supplied prompt images, run commands, use tools, access repositories, or modify the system.";
const AGENT_INSTRUCTIONS: &str = "You are the autonomous Agent inside ChatHead. Work directly in the selected folder, carry tasks through implementation and verification, and use configured MCP servers and discovered skills when useful. You have full filesystem, command, and network access with no approval prompts. Ask the user only when a required choice cannot be inferred safely.";
const MAX_ACTIVITY_BYTES: usize = 64 * 1024;
const MAX_TURN_ACTIVITY_BYTES: usize = 256 * 1024;
const MAX_SERVER_STDERR_BYTES: usize = 8 * 1024;
static RUNTIME_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthenticationState {
    SignedOut,
    ChatGpt,
    ApiKey,
}

#[derive(Debug)]
pub enum CodexCommand {
    SendMessage {
        message_id: String,
        prompt: ChatPrompt,
    },
    Interrupt,
    SwitchMode {
        mode: ChatMode,
        folder: Option<PathBuf>,
    },
    AnswerQuestion {
        request_id: AgentRequestId,
        answers: BTreeMap<String, Vec<String>>,
    },
    NewChat,
    Login,
    Logout,
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexEvent {
    AvailabilityChanged {
        available: bool,
        message: Option<String>,
    },
    AuthenticationChanged(AuthenticationState),
    AuthenticationUrl(String),
    ThreadReady,
    ModeChanged {
        mode: ChatMode,
        folder: Option<PathBuf>,
    },
    ActivityUpsert(AgentActivity),
    QuestionAsked(AgentQuestion),
    QuestionCleared {
        request_id: AgentRequestId,
    },
    AssistantMessageStarted {
        message_id: String,
    },
    AssistantTextDelta {
        message_id: String,
        delta: String,
    },
    TurnCompleted {
        message_id: String,
    },
    TurnInterrupted {
        message_id: String,
    },
    Failure {
        message_id: Option<String>,
        message: String,
        fatal: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CodexServiceError {
    #[error("prompt cannot be empty")]
    EmptyPrompt,
    #[error("a response is already being generated")]
    Busy,
    #[error("experimental Codex chat is unavailable")]
    Unavailable,
}

#[derive(Clone)]
pub struct CodexAppServer {
    commands: mpsc::Sender<CodexCommand>,
    events: Arc<Mutex<mpsc::Receiver<CodexEvent>>>,
}

impl CodexAppServer {
    #[must_use]
    pub fn start() -> Self {
        let (command_tx, command_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let worker_events = event_tx.clone();
        if thread::Builder::new()
            .name("chathead-codex".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => runtime.block_on(worker(command_rx, worker_events)),
                    Err(_) => {
                        // There is no safe recovery path when the worker runtime cannot start.
                    }
                }
            })
            .is_err()
        {
            let _ = event_tx.try_send(CodexEvent::AvailabilityChanged {
                available: false,
                message: Some("Codex worker could not start.".to_owned()),
            });
        }
        Self {
            commands: command_tx,
            events: Arc::new(Mutex::new(event_rx)),
        }
    }

    pub fn send(&self, command: CodexCommand) -> Result<(), CodexServiceError> {
        self.commands
            .try_send(command)
            .map_err(|_| CodexServiceError::Unavailable)
    }

    pub fn try_recv(&self) -> Option<CodexEvent> {
        self.events.lock().ok()?.try_recv().ok()
    }
}

async fn worker(mut commands: mpsc::Receiver<CodexCommand>, events: mpsc::Sender<CodexEvent>) {
    probe_account(&events).await;
    let mut session: Option<Session> = None;

    while let Some(command) = commands.recv().await {
        match command {
            CodexCommand::SendMessage { message_id, prompt } => {
                if prompt.is_empty() {
                    emit_failure(
                        &events,
                        Some(message_id),
                        "Enter a message to continue.",
                        false,
                    )
                    .await;
                    continue;
                }
                if session.is_none() {
                    match Session::start().await {
                        Ok(mut started) => match started.start_thread(ChatMode::Chat, None).await {
                            Ok(()) => {
                                let _ = events.send(CodexEvent::ThreadReady).await;
                                session = Some(started);
                            }
                            Err(_) => {
                                started.shutdown().await;
                                emit_failure(
                                    &events,
                                    Some(message_id),
                                    "Could not start a Codex conversation.",
                                    true,
                                )
                                .await;
                                continue;
                            }
                        },
                        Err(message) => {
                            emit_failure(&events, Some(message_id), &message, true).await;
                            continue;
                        }
                    }
                }

                let Some(active) = session.as_mut() else {
                    continue;
                };
                match active.start_turn(&message_id, &prompt).await {
                    Ok(turn_id) => {
                        let outcome = active
                            .run_turn(&message_id, &turn_id, &mut commands, &events)
                            .await;
                        if outcome == TurnOutcome::Shutdown {
                            active.shutdown().await;
                            return;
                        }
                        if outcome == TurnOutcome::NewChat || outcome == TurnOutcome::Crashed {
                            active.shutdown().await;
                            session = None;
                        }
                    }
                    Err(_) => {
                        emit_failure(
                            &events,
                            Some(message_id),
                            "Codex could not start the response.",
                            false,
                        )
                        .await;
                    }
                }
            }
            CodexCommand::Interrupt => {}
            CodexCommand::SwitchMode { mode, folder } => {
                if mode == ChatMode::Agent && !folder.as_ref().is_some_and(|path| path.is_dir()) {
                    emit_failure(
                        &events,
                        None,
                        "Choose an existing local folder for Agent mode.",
                        false,
                    )
                    .await;
                    continue;
                }
                let mut startup_failure = None;
                if let Some(active) = session.as_mut() {
                    if active.mode == mode && active.folder == folder {
                        let _ = events.send(CodexEvent::ModeChanged { mode, folder }).await;
                        continue;
                    }
                    if active.fork_thread(mode, folder.as_deref()).await.is_err() {
                        emit_failure(
                            &events,
                            None,
                            "Could not switch the Codex conversation context.",
                            false,
                        )
                        .await;
                        continue;
                    }
                } else {
                    match Session::start().await {
                        Ok(mut started) => {
                            if started.start_thread(mode, folder.as_deref()).await.is_ok() {
                                session = Some(started);
                            } else {
                                started.shutdown().await;
                            }
                        }
                        Err(message) => {
                            startup_failure = Some(message);
                        }
                    }
                }
                if session.is_some() {
                    let _ = events.send(CodexEvent::ModeChanged { mode, folder }).await;
                } else {
                    emit_failure(
                        &events,
                        None,
                        startup_failure
                            .as_deref()
                            .unwrap_or("Could not start the requested Codex mode."),
                        false,
                    )
                    .await;
                }
            }
            CodexCommand::AnswerQuestion { .. } => {}
            CodexCommand::NewChat => {
                if let Some(mut old) = session.take() {
                    old.unsubscribe().await;
                    old.shutdown().await;
                }
            }
            CodexCommand::Login => login(&mut commands, &events).await,
            CodexCommand::Logout => {
                logout(&events).await;
                if let Some(mut old) = session.take() {
                    old.shutdown().await;
                }
            }
            CodexCommand::Shutdown => {
                if let Some(mut active) = session.take() {
                    active.shutdown().await;
                }
                return;
            }
        }
    }
    if let Some(mut active) = session {
        active.shutdown().await;
    }
}

async fn probe_account(events: &mpsc::Sender<CodexEvent>) {
    match Session::start().await {
        Ok(mut session) => {
            let _ = events
                .send(CodexEvent::AvailabilityChanged {
                    available: true,
                    message: None,
                })
                .await;
            let state = session
                .account_state()
                .await
                .unwrap_or(AuthenticationState::SignedOut);
            let _ = events.send(CodexEvent::AuthenticationChanged(state)).await;
            session.shutdown().await;
        }
        Err(message) => {
            let _ = events
                .send(CodexEvent::AvailabilityChanged {
                    available: false,
                    message: Some(message),
                })
                .await;
            let _ = events
                .send(CodexEvent::AuthenticationChanged(
                    AuthenticationState::SignedOut,
                ))
                .await;
        }
    }
}

async fn login(commands: &mut mpsc::Receiver<CodexCommand>, events: &mpsc::Sender<CodexEvent>) {
    let mut session = match Session::start().await {
        Ok(session) => session,
        Err(message) => {
            emit_failure(events, None, &message, false).await;
            return;
        }
    };
    let response = session
        .request(
            "account/login/start",
            json!({ "type": "chatgpt", "appBrand": "chatgpt" }),
        )
        .await;
    let Ok(response) = response else {
        emit_failure(
            events,
            None,
            "Could not start ChatGPT authentication.",
            false,
        )
        .await;
        session.shutdown().await;
        return;
    };
    let Some(url) = response.pointer("/result/authUrl").and_then(Value::as_str) else {
        emit_failure(
            events,
            None,
            "Codex returned an invalid authentication response.",
            false,
        )
        .await;
        session.shutdown().await;
        return;
    };
    let _ = events
        .send(CodexEvent::AuthenticationUrl(url.to_owned()))
        .await;

    let completed = timeout(LOGIN_TIMEOUT, async {
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(CodexCommand::Shutdown | CodexCommand::NewChat) | None => return None,
                    _ => {}
                },
                line = session.lines.next_line() => {
                    let Ok(Some(line)) = line else { return Some(false) };
                    let Ok(value) = serde_json::from_str::<Value>(&line) else { return Some(false) };
                    session.reject_server_request(&value).await;
                    if value.get("method").and_then(Value::as_str) == Some("account/login/completed") {
                        return Some(value.pointer("/params/success").and_then(Value::as_bool).unwrap_or(false));
                    }
                }
            }
        }
    })
    .await;

    match completed {
        Ok(Some(true)) => {
            let state = session
                .account_state()
                .await
                .unwrap_or(AuthenticationState::SignedOut);
            let _ = events.send(CodexEvent::AuthenticationChanged(state)).await;
        }
        Ok(None) => {}
        Ok(Some(false)) => {
            emit_failure(
                events,
                None,
                "ChatGPT authentication did not complete.",
                false,
            )
            .await
        }
        Err(_) => {
            emit_failure(
                events,
                None,
                "ChatGPT authentication timed out. Try again.",
                false,
            )
            .await
        }
    }
    session.shutdown().await;
}

async fn logout(events: &mpsc::Sender<CodexEvent>) {
    let Ok(mut session) = Session::start().await else {
        emit_failure(events, None, "Codex logout is unavailable.", false).await;
        return;
    };
    if session.request("account/logout", json!({})).await.is_ok() {
        let _ = events
            .send(CodexEvent::AuthenticationChanged(
                AuthenticationState::SignedOut,
            ))
            .await;
    } else {
        emit_failure(
            events,
            None,
            "Could not disconnect the ChatGPT subscription.",
            false,
        )
        .await;
    }
    session.shutdown().await;
}

async fn emit_failure(
    events: &mpsc::Sender<CodexEvent>,
    message_id: Option<String>,
    message: &str,
    fatal: bool,
) {
    let _ = events
        .send(CodexEvent::Failure {
            message_id,
            message: message.to_owned(),
            fatal,
        })
        .await;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TurnOutcome {
    Complete,
    NewChat,
    Shutdown,
    Crashed,
}

struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Lines<BufReader<ChildStdout>>,
    runtime_dir: PathBuf,
    next_request_id: u64,
    thread_id: Option<String>,
    mode: ChatMode,
    folder: Option<PathBuf>,
    activity_output: BTreeMap<String, String>,
    turn_activity_bytes: usize,
    pending_question_id: Option<AgentRequestId>,
    stderr: Arc<Mutex<String>>,
    stderr_task: Option<JoinHandle<()>>,
}

impl Session {
    async fn start() -> Result<Self, String> {
        let binary = resolve_codex_binary()
            .ok_or_else(|| "Codex CLI executable was not found.".to_owned())?;
        Self::start_binary(&binary).await
    }

    async fn start_binary(binary: &Path) -> Result<Self, String> {
        let runtime_dir = create_runtime_dir().map_err(|error| {
            format!("ChatHead could not create its temporary directory: {error}")
        })?;
        let mut command = Command::new(binary);
        command
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(parent) = binary.parent() {
            let current_path = env::var_os("PATH");
            let paths = std::iter::once(parent.to_path_buf()).chain(
                current_path
                    .as_deref()
                    .into_iter()
                    .flat_map(env::split_paths),
            );
            if let Ok(path) = env::join_paths(paths) {
                command.env("PATH", path);
            }
        }
        let mut child = command.spawn().map_err(|error| {
            let _ = fs::remove_dir_all(&runtime_dir);
            format!("Codex App Server could not be launched: {error}")
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Codex App Server did not provide an input stream.".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Codex App Server did not provide an output stream.".to_owned())?;
        let child_stderr = child
            .stderr
            .take()
            .ok_or_else(|| "Codex App Server did not provide a diagnostic stream.".to_owned())?;
        let stderr = Arc::new(Mutex::new(String::new()));
        let stderr_task = tokio::spawn(drain_server_stderr(child_stderr, Arc::clone(&stderr)));
        let mut session = Self {
            child,
            stdin: Some(stdin),
            lines: BufReader::new(stdout).lines(),
            runtime_dir,
            next_request_id: 0,
            thread_id: None,
            mode: ChatMode::Chat,
            folder: None,
            activity_output: BTreeMap::new(),
            turn_activity_bytes: 0,
            pending_question_id: None,
            stderr,
            stderr_task: Some(stderr_task),
        };
        let initialized = timeout(
            HANDSHAKE_TIMEOUT,
            session.request(
                "initialize",
                json!({
                    "clientInfo": { "name": "chathead-ai", "title": "ChatHead AI", "version": env!("CARGO_PKG_VERSION") },
                    "capabilities": { "experimentalApi": true }
                }),
            ),
        )
        .await;
        if !matches!(initialized, Ok(Ok(_))) {
            session.shutdown().await;
            return Err(startup_failure_message(&session.stderr_snapshot()));
        }
        session
            .notify("initialized", json!({}))
            .await
            .map_err(|_| startup_failure_message(&session.stderr_snapshot()))?;
        Ok(session)
    }

    fn stderr_snapshot(&self) -> String {
        self.stderr
            .lock()
            .map(|stderr| stderr.clone())
            .unwrap_or_default()
    }

    async fn account_state(&mut self) -> Result<AuthenticationState, ()> {
        let value = self
            .request("account/read", json!({ "refreshToken": false }))
            .await?;
        Ok(
            match value
                .pointer("/result/account/type")
                .and_then(Value::as_str)
            {
                Some("chatgpt") => AuthenticationState::ChatGpt,
                Some("apiKey") => AuthenticationState::ApiKey,
                _ => AuthenticationState::SignedOut,
            },
        )
    }

    async fn start_thread(&mut self, mode: ChatMode, folder: Option<&Path>) -> Result<(), ()> {
        let cwd = context_cwd(mode, folder, &self.runtime_dir)?;
        let value = self
            .request("thread/start", thread_start_params(mode, cwd))
            .await?;
        self.thread_id = value
            .pointer("/result/thread/id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.thread_id.as_ref().map(|_| ()).ok_or(())?;
        self.mode = mode;
        self.folder = folder.map(Path::to_path_buf);
        Ok(())
    }

    async fn fork_thread(&mut self, mode: ChatMode, folder: Option<&Path>) -> Result<(), ()> {
        let old_thread_id = self.thread_id.as_deref().ok_or(())?;
        let value = self
            .request(
                "thread/fork",
                thread_fork_params(
                    mode,
                    old_thread_id,
                    context_cwd(mode, folder, &self.runtime_dir)?,
                ),
            )
            .await?;
        let new_thread_id = value
            .pointer("/result/thread/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(())?;
        self.thread_id = Some(new_thread_id);
        self.mode = mode;
        self.folder = folder.map(Path::to_path_buf);
        Ok(())
    }

    async fn start_turn(&mut self, message_id: &str, prompt: &ChatPrompt) -> Result<String, ()> {
        self.activity_output.clear();
        self.turn_activity_bytes = 0;
        let thread_id = self.thread_id.as_deref().ok_or(())?;
        let cwd = context_cwd(self.mode, self.folder.as_deref(), &self.runtime_dir)?;
        let sandbox_policy = match self.mode {
            ChatMode::Chat => json!({ "type": "readOnly", "networkAccess": false }),
            ChatMode::Agent => json!({ "type": "dangerFullAccess" }),
        };
        let value = self
            .request(
                "turn/start",
                turn_start_params(thread_id, message_id, prompt, cwd, sandbox_policy),
            )
            .await?;
        value
            .pointer("/result/turn/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(())
    }

    async fn run_turn(
        &mut self,
        message_id: &str,
        turn_id: &str,
        commands: &mut mpsc::Receiver<CodexCommand>,
        events: &mpsc::Sender<CodexEvent>,
    ) -> TurnOutcome {
        let mut started = false;
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(CodexCommand::Interrupt) => {
                        self.interrupt(turn_id).await;
                        if let Some(request_id) = self.pending_question_id.take() {
                            let _ = events.send(CodexEvent::QuestionCleared { request_id }).await;
                        }
                    }
                    Some(CodexCommand::NewChat) => {
                        self.interrupt(turn_id).await;
                        let _ = events.send(CodexEvent::TurnInterrupted { message_id: message_id.to_owned() }).await;
                        self.unsubscribe().await;
                        return TurnOutcome::NewChat;
                    }
                    Some(CodexCommand::Shutdown) | None => {
                        self.interrupt(turn_id).await;
                        return TurnOutcome::Shutdown;
                    }
                    Some(CodexCommand::SendMessage { message_id, .. }) => {
                        emit_failure(events, Some(message_id), "Wait for the current response or stop it first.", false).await;
                    }
                    Some(CodexCommand::SwitchMode { .. } | CodexCommand::Login | CodexCommand::Logout) => {}
                    Some(CodexCommand::AnswerQuestion { request_id, answers }) => {
                        let _ = self.answer_question(&request_id, &answers).await;
                        self.pending_question_id = None;
                        let _ = events.send(CodexEvent::QuestionCleared { request_id }).await;
                    }
                },
                line = self.lines.next_line() => {
                    let Ok(Some(line)) = line else {
                        emit_failure(events, Some(message_id.to_owned()), "Codex stopped unexpectedly. Start a new chat to retry.", true).await;
                        return TurnOutcome::Crashed;
                    };
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        emit_failure(events, Some(message_id.to_owned()), "Codex returned malformed protocol data.", true).await;
                        return TurnOutcome::Crashed;
                    };
                    if self.handle_server_request(&value, turn_id, events).await {
                        continue;
                    }
                    let method = value.get("method").and_then(Value::as_str);
                    let same_turn = notification_turn_id(&value) == Some(turn_id);
                    match method {
                        Some("item/started") if same_turn
                            && value.pointer("/params/item/type").and_then(Value::as_str)
                                == Some("agentMessage") =>
                        {
                            started = true;
                            let _ = events.send(CodexEvent::AssistantMessageStarted { message_id: message_id.to_owned() }).await;
                        }
                        Some("item/agentMessage/delta") if same_turn => {
                            if !started {
                                started = true;
                                let _ = events.send(CodexEvent::AssistantMessageStarted { message_id: message_id.to_owned() }).await;
                            }
                            if let Some(delta) = value.pointer("/params/delta").and_then(Value::as_str) {
                                let _ = events.send(CodexEvent::AssistantTextDelta { message_id: message_id.to_owned(), delta: delta.to_owned() }).await;
                            }
                        }
                        Some("item/started" | "item/completed") if same_turn && self.mode == ChatMode::Agent => {
                            if let Some(activity) = self.activity_from_item(&value) {
                                let _ = events.send(CodexEvent::ActivityUpsert(activity)).await;
                            }
                        }
                        Some("serverRequest/resolved") => {
                            if let Some(request_id) = value.pointer("/params/requestId").and_then(request_id_from_value) {
                                self.pending_question_id = None;
                                let _ = events.send(CodexEvent::QuestionCleared { request_id }).await;
                            }
                        }
                        Some("item/commandExecution/outputDelta") if same_turn && self.mode == ChatMode::Agent => {
                            if let Some(activity) = self.activity_from_command_delta(&value) {
                                let _ = events.send(CodexEvent::ActivityUpsert(activity)).await;
                            }
                        }
                        Some("turn/completed") if same_turn => {
                            let status = value.pointer("/params/turn/status").and_then(Value::as_str);
                            let event = if status == Some("interrupted") {
                                CodexEvent::TurnInterrupted { message_id: message_id.to_owned() }
                            } else if status == Some("completed") {
                                CodexEvent::TurnCompleted { message_id: message_id.to_owned() }
                            } else {
                                CodexEvent::Failure { message_id: Some(message_id.to_owned()), message: "Codex could not complete the response.".to_owned(), fatal: false }
                            };
                            let _ = events.send(event).await;
                            return TurnOutcome::Complete;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    async fn interrupt(&mut self, turn_id: &str) {
        if let Some(thread_id) = self.thread_id.clone() {
            let _ = self
                .send_request(
                    "turn/interrupt",
                    json!({ "threadId": thread_id, "turnId": turn_id }),
                )
                .await;
        }
    }

    async fn answer_question(
        &mut self,
        request_id: &AgentRequestId,
        answers: &BTreeMap<String, Vec<String>>,
    ) -> Result<(), ()> {
        let answers = answers
            .iter()
            .map(|(id, answers)| (id.clone(), json!({ "answers": answers })))
            .collect::<serde_json::Map<_, _>>();
        self.write(&json!({ "id": request_id, "result": { "answers": answers } }))
            .await
    }

    async fn handle_server_request(
        &mut self,
        value: &Value,
        turn_id: &str,
        events: &mpsc::Sender<CodexEvent>,
    ) -> bool {
        let Some(id) = value.get("id").and_then(request_id_from_value) else {
            return false;
        };
        let Some(method) = value.get("method").and_then(Value::as_str) else {
            return false;
        };
        if notification_turn_id(value).is_some_and(|id| id != turn_id) {
            return false;
        }
        if method == "tool/requestUserInput" || method == "item/tool/requestUserInput" {
            if let Some(question) = question_from_request(id.clone(), value) {
                self.pending_question_id = Some(id);
                let _ = events.send(CodexEvent::QuestionAsked(question)).await;
            } else {
                let _ = self
                    .write(&json!({ "id": id, "error": { "code": -32602, "message": "invalid user input request" } }))
                    .await;
            }
            return true;
        }
        self.reject_server_request(value).await;
        true
    }

    fn activity_from_item(&mut self, value: &Value) -> Option<AgentActivity> {
        let item = value.pointer("/params/item")?;
        let item_type = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if matches!(
            item_type,
            "userMessage" | "agentMessage" | "reasoning" | "plan"
        ) {
            return None;
        }
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(item_type)
            .to_owned();
        let kind = match item_type {
            "commandExecution" => AgentActivityKind::Command,
            "fileChange" => AgentActivityKind::FileChange,
            "mcpToolCall" | "dynamicToolCall" => AgentActivityKind::McpCall,
            "webSearch" => AgentActivityKind::WebSearch,
            _ => AgentActivityKind::Unknown,
        };
        let title = activity_title(kind, item);
        let detail = serde_json::to_string_pretty(item).unwrap_or_else(|_| item_type.to_owned());
        let (detail, truncated) = self.bound_activity_detail(&detail);
        Some(AgentActivity {
            id,
            kind,
            title,
            detail,
            status: item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or(
                    if value.get("method").and_then(Value::as_str) == Some("item/completed") {
                        "completed"
                    } else {
                        "inProgress"
                    },
                )
                .to_owned(),
            truncated,
        })
    }

    fn activity_from_command_delta(&mut self, value: &Value) -> Option<AgentActivity> {
        let id = value
            .pointer("/params/itemId")
            .and_then(Value::as_str)?
            .to_owned();
        let delta = value
            .pointer("/params/delta")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (detail, item_truncated) = {
            let output = self.activity_output.entry(id.clone()).or_default();
            if output.len() < MAX_ACTIVITY_BYTES {
                let remaining = MAX_ACTIVITY_BYTES - output.len();
                output.push_str(truncate_utf8(delta, remaining).0);
            }
            (output.clone(), output.len() >= MAX_ACTIVITY_BYTES)
        };
        let (detail, turn_truncated) = self.bound_activity_detail(&detail);
        Some(AgentActivity {
            id,
            kind: AgentActivityKind::Command,
            title: "Command output".to_owned(),
            detail,
            status: "inProgress".to_owned(),
            truncated: turn_truncated || item_truncated,
        })
    }

    fn bound_activity_detail(&mut self, detail: &str) -> (String, bool) {
        let item_limit = MAX_ACTIVITY_BYTES
            .min(MAX_TURN_ACTIVITY_BYTES.saturating_sub(self.turn_activity_bytes));
        let (bounded, truncated) = truncate_utf8(detail, item_limit);
        self.turn_activity_bytes = self.turn_activity_bytes.saturating_add(bounded.len());
        let mut result = bounded.to_owned();
        if truncated {
            result.push_str("\n… output truncated …");
        }
        (result, truncated)
    }

    async fn unsubscribe(&mut self) {
        if let Some(thread_id) = self.thread_id.take() {
            let _ = self
                .send_request("thread/unsubscribe", json!({ "threadId": thread_id }))
                .await;
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, ()> {
        let id = self.send_request(method, params).await?;
        loop {
            let line = self.lines.next_line().await.map_err(|_| ())?.ok_or(())?;
            let value: Value = serde_json::from_str(&line).map_err(|_| ())?;
            self.reject_server_request(&value).await;
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return if value.get("error").is_some() {
                    Err(())
                } else {
                    Ok(value)
                };
            }
        }
    }

    async fn send_request(&mut self, method: &str, params: Value) -> Result<u64, ()> {
        self.next_request_id = self.next_request_id.saturating_add(1);
        let id = self.next_request_id;
        self.write(&json!({ "id": id, "method": method, "params": params }))
            .await?;
        Ok(id)
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), ()> {
        self.write(&json!({ "method": method, "params": params }))
            .await
    }

    async fn write(&mut self, value: &Value) -> Result<(), ()> {
        let stdin = self.stdin.as_mut().ok_or(())?;
        let mut line = serde_json::to_vec(value).map_err(|_| ())?;
        line.push(b'\n');
        stdin.write_all(&line).await.map_err(|_| ())?;
        stdin.flush().await.map_err(|_| ())
    }

    async fn reject_server_request(&mut self, value: &Value) {
        let Some(id) = value.get("id").cloned() else {
            return;
        };
        if value.get("method").is_none() {
            return;
        }
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let response = negative_server_response(id, method);
        let _ = self.write(&response).await;
    }

    async fn shutdown(&mut self) {
        self.stdin.take();
        if timeout(SHUTDOWN_TIMEOUT, self.child.wait()).await.is_err() {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
        if let Some(stderr_task) = self.stderr_task.take() {
            let _ = timeout(SHUTDOWN_TIMEOUT, stderr_task).await;
        }
        let _ = fs::remove_dir_all(&self.runtime_dir);
    }
}

async fn drain_server_stderr(mut stderr: ChildStderr, output: Arc<Mutex<String>>) {
    let mut chunk = [0_u8; 4096];
    loop {
        let Ok(count) = stderr.read(&mut chunk).await else {
            return;
        };
        if count == 0 {
            return;
        }
        let Ok(mut output) = output.lock() else {
            return;
        };
        let remaining = MAX_SERVER_STDERR_BYTES.saturating_sub(output.len());
        if remaining > 0 {
            let decoded = String::from_utf8_lossy(&chunk[..count]);
            output.push_str(truncate_utf8(&decoded, remaining).0);
        }
    }
}

fn startup_failure_message(stderr: &str) -> String {
    let stderr = stderr.trim();
    if stderr.contains("failed to initialize sqlite state runtime") {
        return "Codex could not initialize its local state. Free disk space and verify that ~/.codex is writable."
            .to_owned();
    }
    if stderr.is_empty() {
        "Codex App Server could not start.".to_owned()
    } else {
        format!("Codex App Server could not start: {stderr}")
    }
}

fn negative_server_response(id: Value, method: &str) -> Value {
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            json!({ "id": id, "result": { "decision": "decline" } })
        }
        "item/permissions/requestApproval" => {
            json!({ "id": id, "result": { "permissions": {}, "scope": "turn" } })
        }
        "mcpServer/elicitation/request" => {
            json!({ "id": id, "result": { "action": "decline", "content": null, "_meta": null } })
        }
        _ => json!({
            "id": id,
            "error": { "code": -32601, "message": "unsupported server request" }
        }),
    }
}

fn instructions(mode: ChatMode) -> &'static str {
    match mode {
        ChatMode::Chat => DEVELOPER_INSTRUCTIONS,
        ChatMode::Agent => AGENT_INSTRUCTIONS,
    }
}

fn context_cwd<'a>(
    mode: ChatMode,
    folder: Option<&'a Path>,
    runtime_dir: &'a Path,
) -> Result<&'a str, ()> {
    let path = match mode {
        ChatMode::Chat => runtime_dir,
        ChatMode::Agent => folder.filter(|path| path.is_dir()).ok_or(())?,
    };
    path.to_str().ok_or(())
}

fn thread_start_params(mode: ChatMode, cwd: &str) -> Value {
    json!({
        "cwd": cwd,
        "ephemeral": true,
        "approvalPolicy": "never",
        "sandbox": sandbox_mode(mode),
        "developerInstructions": instructions(mode)
    })
}

fn thread_fork_params(mode: ChatMode, thread_id: &str, cwd: &str) -> Value {
    json!({
        "threadId": thread_id,
        "ephemeral": true,
        "cwd": cwd,
        "approvalPolicy": "never",
        "sandbox": sandbox_mode(mode),
        "developerInstructions": instructions(mode)
    })
}

fn sandbox_mode(mode: ChatMode) -> &'static str {
    match mode {
        ChatMode::Chat => "read-only",
        ChatMode::Agent => "danger-full-access",
    }
}

fn turn_start_params(
    thread_id: &str,
    message_id: &str,
    prompt: &ChatPrompt,
    cwd: &str,
    sandbox_policy: Value,
) -> Value {
    json!({
        "threadId": thread_id,
        "clientUserMessageId": message_id,
        "input": turn_input(prompt),
        "cwd": cwd,
        "approvalPolicy": "never",
        "sandboxPolicy": sandbox_policy
    })
}

fn activity_title(kind: AgentActivityKind, item: &Value) -> String {
    match kind {
        AgentActivityKind::Command => item
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("Command")
            .to_owned(),
        AgentActivityKind::FileChange => {
            let count = item
                .get("changes")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            format!("File changes · {count}")
        }
        AgentActivityKind::McpCall => {
            let server = item.get("server").and_then(Value::as_str).unwrap_or("MCP");
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("tool");
            format!("{server} · {tool}")
        }
        AgentActivityKind::WebSearch => item
            .get("query")
            .and_then(Value::as_str)
            .map_or_else(|| "Web search".to_owned(), |query| format!("Web · {query}")),
        AgentActivityKind::Unknown => item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("Agent activity")
            .to_owned(),
    }
}

fn question_from_request(request_id: AgentRequestId, value: &Value) -> Option<AgentQuestion> {
    let questions = value.pointer("/params/questions")?.as_array()?;
    if questions.is_empty() || questions.len() > 3 {
        return None;
    }
    let fields = questions
        .iter()
        .map(|question| {
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|option| {
                    Some(AgentQuestionOption {
                        label: option.get("label")?.as_str()?.to_owned(),
                        description: option
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    })
                })
                .collect();
            Some(AgentQuestionField {
                id: question.get("id")?.as_str()?.to_owned(),
                header: question
                    .get("header")
                    .and_then(Value::as_str)
                    .unwrap_or("Question")
                    .to_owned(),
                question: question.get("question")?.as_str()?.to_owned(),
                options,
                allow_other: question
                    .get("isOther")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                secret: question
                    .get("isSecret")
                    .or_else(|| question.get("secret"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(AgentQuestion { request_id, fields })
}

fn request_id_from_value(value: &Value) -> Option<AgentRequestId> {
    value.as_i64().map(AgentRequestId::Number).or_else(|| {
        value
            .as_str()
            .map(|id| AgentRequestId::String(id.to_owned()))
    })
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (&str, bool) {
    if value.len() <= max_bytes {
        return (value, false);
    }
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (&value[..end], true)
}

fn turn_input(prompt: &ChatPrompt) -> Vec<Value> {
    let mut input =
        Vec::with_capacity(usize::from(!prompt.text.trim().is_empty()) + prompt.attachments.len());
    if !prompt.text.trim().is_empty() {
        input.push(json!({ "type": "text", "text": prompt.text.trim() }));
    }
    input.extend(prompt.attachments.iter().map(|attachment| {
        json!({
            "type": "localImage",
            "path": attachment.path.to_string_lossy()
        })
    }));
    input
}

fn notification_turn_id(value: &Value) -> Option<&str> {
    value
        .pointer("/params/turnId")
        .or_else(|| value.pointer("/params/turn/id"))
        .and_then(Value::as_str)
}

fn create_runtime_dir() -> std::io::Result<PathBuf> {
    let sequence = RUNTIME_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = env::temp_dir().join(format!(
        "chathead-ai-runtime-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir(&path)?;
    Ok(path)
}

fn resolve_codex_binary() -> Option<PathBuf> {
    resolve_codex_binary_from(
        env::var_os(CODEX_BINARY_ENV).as_deref(),
        env::var_os("PATH").as_deref(),
        env::var_os("HOME").as_deref().map(Path::new),
    )
}

fn resolve_codex_binary_from(
    explicit: Option<&OsStr>,
    path: Option<&OsStr>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(candidate) = explicit.map(PathBuf::from).filter(|path| path.is_file()) {
        return Some(candidate);
    }
    if let Some(candidate) = path.and_then(|value| {
        env::split_paths(value)
            .map(|directory| directory.join("codex"))
            .find(|candidate| candidate.is_file())
    }) {
        return Some(candidate);
    }
    let home = home?;
    let local = home.join(".local/bin/codex");
    if local.is_file() {
        return Some(local);
    }
    fs::read_dir(home.join(".nvm/versions/node"))
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let version = parse_node_version(&entry.file_name())?;
            let candidate = entry.path().join("bin/codex");
            candidate.is_file().then_some((version, candidate))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path)
}

fn parse_node_version(value: &OsStr) -> Option<(u32, u32, u32)> {
    let mut parts = value.to_str()?.strip_prefix('v')?.split('.');
    let result = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn fake_server(script_body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let sequence = RUNTIME_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "chathead-fake-codex-{}-{sequence}.sh",
            std::process::id()
        ));
        fs::write(&path, format!("#!/bin/sh\n{script_body}\n")).expect("write fake server");
        let mut permissions = fs::metadata(&path).expect("fake metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).expect("make fake executable");
        path
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn fake_transport_enforces_handshake_order_and_typed_account_read() {
        let binary = fake_server(concat!(
            "while IFS= read -r line; do\n",
            "case \"$line\" in\n",
            "  *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}' ;;\n",
            "  *'\"method\":\"account/read\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"account\":{\"type\":\"chatgpt\",\"email\":\"not-forwarded@example.com\"},\"requiresOpenaiAuth\":true}}' ;;\n",
            "esac\n",
            "done",
        ));

        let mut session = Session::start_binary(&binary).await.expect("handshake");
        assert_eq!(
            session.account_state().await.expect("account read"),
            AuthenticationState::ChatGpt
        );
        session.shutdown().await;
        let _ = fs::remove_file(binary);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn malformed_json_during_handshake_is_recoverable() {
        let binary = fake_server("IFS= read -r line\nprintf '%s\\n' 'not-json'");

        assert!(Session::start_binary(&binary).await.is_err());
        let _ = fs::remove_file(binary);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn startup_stderr_is_bounded_and_mapped_to_an_actionable_error() {
        let binary = fake_server(concat!(
            "printf '%s\\n' 'Error: failed to initialize sqlite state runtime under /tmp/codex' >&2\n",
            "exit 1",
        ));

        let error = match Session::start_binary(&binary).await {
            Ok(mut session) => {
                session.shutdown().await;
                panic!("startup should fail");
            }
            Err(error) => error,
        };
        assert_eq!(
            error,
            "Codex could not initialize its local state. Free disk space and verify that ~/.codex is writable."
        );
        let _ = fs::remove_file(binary);
    }

    #[test]
    fn generic_startup_diagnostics_remain_bounded() {
        let diagnostic = "x".repeat(MAX_SERVER_STDERR_BYTES);
        let message = startup_failure_message(&diagnostic);

        assert!(message.ends_with(&diagnostic));
        assert!(message.len() <= MAX_SERVER_STDERR_BYTES + 40);
    }

    #[test]
    fn account_parser_does_not_require_or_expose_email() {
        let chatgpt = json!({ "result": { "account": { "type": "chatgpt", "email": "private@example.com" } } });
        let api_key = json!({ "result": { "account": { "type": "apiKey" } } });
        let signed_out = json!({ "result": { "account": null } });
        let parse = |value: &Value| match value
            .pointer("/result/account/type")
            .and_then(Value::as_str)
        {
            Some("chatgpt") => AuthenticationState::ChatGpt,
            Some("apiKey") => AuthenticationState::ApiKey,
            _ => AuthenticationState::SignedOut,
        };
        assert_eq!(parse(&chatgpt), AuthenticationState::ChatGpt);
        assert_eq!(parse(&api_key), AuthenticationState::ApiKey);
        assert_eq!(parse(&signed_out), AuthenticationState::SignedOut);
    }

    #[test]
    fn conversation_payload_is_ephemeral_and_uses_no_model_override() {
        let payload = thread_start_params(ChatMode::Chat, "/tmp/empty");
        assert_eq!(payload["ephemeral"], true);
        assert_eq!(payload["sandbox"], "read-only");
        assert!(payload.get("model").is_none());
        assert!(!payload.to_string().contains("thread/rollback"));
        assert!(payload.get("environments").is_none());
        assert!(payload.get("runtimeWorkspaceRoots").is_none());
    }

    #[test]
    fn chat_and_agent_turn_payloads_apply_exact_permission_boundaries() {
        let prompt = ChatPrompt::text("do it");
        let chat = turn_start_params(
            "thread-1",
            "message-1",
            &prompt,
            "/tmp/chat",
            json!({ "type": "readOnly", "networkAccess": false }),
        );
        assert_eq!(chat["cwd"], "/tmp/chat");
        assert_eq!(chat["approvalPolicy"], "never");
        assert_eq!(chat["sandboxPolicy"]["type"], "readOnly");
        assert_eq!(chat["sandboxPolicy"]["networkAccess"], false);
        assert!(chat.get("developerInstructions").is_none());

        let agent = turn_start_params(
            "thread-2",
            "message-2",
            &prompt,
            "/work/project",
            json!({ "type": "dangerFullAccess" }),
        );
        assert_eq!(agent["cwd"], "/work/project");
        assert_eq!(agent["approvalPolicy"], "never");
        assert_eq!(
            agent["sandboxPolicy"],
            json!({ "type": "dangerFullAccess" })
        );
        assert!(agent.get("developerInstructions").is_none());
        assert!(!agent.to_string().contains("environments"));
        assert!(!agent.to_string().contains("runtimeWorkspaceRoots"));

        let fork = thread_fork_params(ChatMode::Agent, "thread-1", "/work/project");
        assert_eq!(fork["ephemeral"], true);
        assert_eq!(fork["cwd"], "/work/project");
        assert_eq!(fork["sandbox"], "danger-full-access");
        assert_eq!(fork["developerInstructions"], AGENT_INSTRUCTIONS);

        let start = thread_start_params(ChatMode::Agent, "/work/project");
        assert_eq!(start["sandbox"], "danger-full-access");
    }

    #[test]
    fn approval_requests_fail_closed_with_method_specific_results() {
        assert_eq!(
            negative_server_response(json!(7), "item/commandExecution/requestApproval"),
            json!({ "id": 7, "result": { "decision": "decline" } })
        );
        assert_eq!(
            negative_server_response(json!(8), "item/fileChange/requestApproval"),
            json!({ "id": 8, "result": { "decision": "decline" } })
        );
        assert_eq!(
            negative_server_response(json!(9), "item/permissions/requestApproval"),
            json!({ "id": 9, "result": { "permissions": {}, "scope": "turn" } })
        );
        assert_eq!(
            negative_server_response(json!(10), "future/request"),
            json!({ "id": 10, "error": { "code": -32601, "message": "unsupported server request" } })
        );
    }

    #[test]
    fn parses_options_free_form_and_secret_questions() {
        let request = json!({
            "id": 41,
            "method": "tool/requestUserInput",
            "params": { "questions": [{
                "id": "token",
                "header": "Credential",
                "question": "Enter token",
                "isOther": true,
                "isSecret": true,
                "options": [{ "label": "Use default", "description": "Use configured token" }]
            }] }
        });
        let question =
            question_from_request(AgentRequestId::Number(41), &request).expect("valid question");
        assert_eq!(question.request_id, AgentRequestId::Number(41));
        assert!(question.fields[0].secret);
        assert!(question.fields[0].allow_other);
        assert_eq!(question.fields[0].options[0].label, "Use default");
    }

    #[test]
    fn bounded_details_preserve_utf8_and_mark_truncation() {
        let input = "é".repeat(MAX_ACTIVITY_BYTES);
        let (bounded, truncated) = truncate_utf8(&input, MAX_ACTIVITY_BYTES - 1);
        assert!(truncated);
        assert!(bounded.is_char_boundary(bounded.len()));
        assert!(bounded.len() < MAX_ACTIVITY_BYTES);
    }

    #[test]
    fn request_ids_preserve_numeric_and_string_wire_types() {
        assert_eq!(
            request_id_from_value(&json!(4)),
            Some(AgentRequestId::Number(4))
        );
        assert_eq!(
            request_id_from_value(&json!("request-4")),
            Some(AgentRequestId::String("request-4".to_owned()))
        );
    }

    #[test]
    fn turn_input_keeps_text_and_local_images_in_one_prompt() {
        let prompt = ChatPrompt {
            text: "  describe this  ".to_owned(),
            attachments: vec![crate::ChatAttachment {
                id: "attachment-1".to_owned(),
                path: PathBuf::from("/tmp/attachment-1.png"),
                mime_type: "image/png".to_owned(),
                width: 10,
                height: 20,
                byte_len: 30,
            }],
        };

        assert_eq!(
            turn_input(&prompt),
            vec![
                json!({ "type": "text", "text": "describe this" }),
                json!({ "type": "localImage", "path": "/tmp/attachment-1.png" })
            ]
        );
    }

    #[test]
    fn reads_turn_id_from_delta_and_completed_notifications() {
        let delta = json!({
            "method": "item/agentMessage/delta",
            "params": { "turnId": "turn-1", "delta": "hello" }
        });
        let completed = json!({
            "method": "turn/completed",
            "params": { "turn": { "id": "turn-1", "status": "completed" } }
        });

        assert_eq!(notification_turn_id(&delta), Some("turn-1"));
        assert_eq!(notification_turn_id(&completed), Some("turn-1"));
    }

    #[test]
    fn parses_only_complete_semver_node_directories() {
        assert_eq!(parse_node_version(OsStr::new("v24.1.2")), Some((24, 1, 2)));
        assert_eq!(parse_node_version(OsStr::new("24.1.2")), None);
    }
}
