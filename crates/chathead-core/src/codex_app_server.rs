//! Minimal typed client for the experimental Codex app-server JSONL protocol.

use std::{
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

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::mpsc,
    time::timeout,
};

const CODEX_BINARY_ENV: &str = "CHATHEAD_CODEX_BIN";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const CHANNEL_CAPACITY: usize = 64;
const DEVELOPER_INSTRUCTIONS: &str = "You are a conversational assistant inside ChatHead. Answer the user's questions directly. Do not inspect files, run commands, use tools, access repositories, or modify the system. If asked to perform those actions, explain that this experimental chat is text-only.";
static RUNTIME_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthenticationState {
    SignedOut,
    ChatGpt,
    ApiKey,
}

#[derive(Debug)]
pub enum CodexCommand {
    SendMessage { message_id: String, text: String },
    Interrupt,
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
            CodexCommand::SendMessage { message_id, text } => {
                if text.trim().is_empty() {
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
                        Ok(mut started) => match started.start_thread().await {
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
                        Err(_) => {
                            emit_failure(
                                &events,
                                Some(message_id),
                                "Codex is unavailable. Start a new chat to retry.",
                                true,
                            )
                            .await;
                            continue;
                        }
                    }
                }

                let Some(active) = session.as_mut() else {
                    continue;
                };
                match active.start_turn(&message_id, text.trim()).await {
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
        Err(_) => {
            let _ = events
                .send(CodexEvent::AvailabilityChanged {
                    available: false,
                    message: Some("Codex CLI is unavailable.".to_owned()),
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
    let Ok(mut session) = Session::start().await else {
        emit_failure(events, None, "Codex authentication is unavailable.", false).await;
        return;
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
}

impl Session {
    async fn start() -> Result<Self, ()> {
        let binary = resolve_codex_binary().ok_or(())?;
        Self::start_binary(&binary).await
    }

    async fn start_binary(binary: &Path) -> Result<Self, ()> {
        let runtime_dir = create_runtime_dir().map_err(|_| ())?;
        let mut command = Command::new(binary);
        command
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
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
        let mut child = command.spawn().map_err(|_| ())?;
        let stdin = child.stdin.take().ok_or(())?;
        let stdout = child.stdout.take().ok_or(())?;
        let mut session = Self {
            child,
            stdin: Some(stdin),
            lines: BufReader::new(stdout).lines(),
            runtime_dir,
            next_request_id: 0,
            thread_id: None,
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
            return Err(());
        }
        session.notify("initialized", json!({})).await?;
        Ok(session)
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

    async fn start_thread(&mut self) -> Result<(), ()> {
        let cwd = self.runtime_dir.to_str().ok_or(())?;
        let value = self
            .request(
                "thread/start",
                json!({
                    "cwd": cwd,
                    "ephemeral": true,
                    "approvalPolicy": "never",
                    "developerInstructions": DEVELOPER_INSTRUCTIONS,
                    "environments": [],
                    "runtimeWorkspaceRoots": []
                }),
            )
            .await?;
        self.thread_id = value
            .pointer("/result/thread/id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.thread_id.as_ref().map(|_| ()).ok_or(())
    }

    async fn start_turn(&mut self, message_id: &str, text: &str) -> Result<String, ()> {
        let thread_id = self.thread_id.as_deref().ok_or(())?;
        let value = self
            .request(
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "clientUserMessageId": message_id,
                    "input": [{ "type": "text", "text": text }],
                    "approvalPolicy": "never",
                    "sandboxPolicy": { "type": "readOnly", "networkAccess": false },
                    "environments": [],
                    "runtimeWorkspaceRoots": []
                }),
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
                    Some(CodexCommand::Interrupt) => self.interrupt(turn_id).await,
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
                    Some(CodexCommand::Login | CodexCommand::Logout) => {}
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
                    self.reject_server_request(&value).await;
                    let method = value.get("method").and_then(Value::as_str);
                    let same_turn = notification_turn_id(&value) == Some(turn_id);
                    match method {
                        Some("item/agentMessage/delta") if same_turn => {
                            if !started {
                                started = true;
                                let _ = events.send(CodexEvent::AssistantMessageStarted { message_id: message_id.to_owned() }).await;
                            }
                            if let Some(delta) = value.pointer("/params/delta").and_then(Value::as_str) {
                                let _ = events.send(CodexEvent::AssistantTextDelta { message_id: message_id.to_owned(), delta: delta.to_owned() }).await;
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
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        Ok(id)
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), ()> {
        self.write(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
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
        let _ = self
            .write(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "interactive approvals are disabled" }
            }))
            .await;
    }

    async fn shutdown(&mut self) {
        self.stdin.take();
        if timeout(SHUTDOWN_TIMEOUT, self.child.wait()).await.is_err() {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
        let _ = fs::remove_dir_all(&self.runtime_dir);
    }
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
        let payload = json!({
            "cwd": "/tmp/empty",
            "ephemeral": true,
            "approvalPolicy": "never",
            "developerInstructions": DEVELOPER_INSTRUCTIONS
        });
        assert_eq!(payload["ephemeral"], true);
        assert!(payload.get("model").is_none());
        assert!(!payload.to_string().contains("thread/rollback"));
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
