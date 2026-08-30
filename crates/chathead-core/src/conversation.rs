//! UI-independent reducer for the single experimental conversation.

use crate::{CodexEvent, CodexServiceError};
use std::path::PathBuf;

use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChatMode {
    #[default]
    Chat,
    Agent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentActivityKind {
    Command,
    FileChange,
    McpCall,
    WebSearch,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentActivity {
    pub id: String,
    pub kind: AgentActivityKind,
    pub title: String,
    pub detail: String,
    pub status: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentQuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentQuestionField {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<AgentQuestionOption>,
    pub allow_other: bool,
    pub secret: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AgentRequestId {
    Number(i64),
    String(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentQuestion {
    pub request_id: AgentRequestId,
    pub fields: Vec<AgentQuestionField>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachment {
    pub id: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub byte_len: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPrompt {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ChatAttachment>,
}

impl ChatPrompt {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            attachments: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.attachments.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageState {
    Complete,
    Streaming,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub role: MessageRole,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ChatAttachment>,
    pub state: MessageState,
}

#[derive(Debug, Default)]
pub struct Conversation {
    messages: Vec<ChatMessage>,
    active_message_id: Option<String>,
    last_prompt: Option<ChatPrompt>,
    next_id: u64,
    mode: ChatMode,
    agent_folder: Option<PathBuf>,
    activities: Vec<AgentActivity>,
    question: Option<AgentQuestion>,
}

impl Conversation {
    #[must_use]
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.active_message_id.is_some()
    }

    #[must_use]
    pub fn mode(&self) -> ChatMode {
        self.mode
    }

    #[must_use]
    pub fn agent_folder(&self) -> Option<&std::path::Path> {
        self.agent_folder.as_deref()
    }

    #[must_use]
    pub fn activities(&self) -> &[AgentActivity] {
        &self.activities
    }

    #[must_use]
    pub fn question(&self) -> Option<&AgentQuestion> {
        self.question.as_ref()
    }

    #[must_use]
    pub fn last_prompt(&self) -> Option<&ChatPrompt> {
        self.last_prompt.as_ref()
    }

    #[must_use]
    pub fn prompt_for_assistant(&self, assistant_message_id: &str) -> Option<ChatPrompt> {
        let assistant_index = self
            .messages
            .iter()
            .position(|message| message.id == assistant_message_id)?;
        self.messages[..assistant_index]
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
            .map(|message| ChatPrompt {
                text: message.text.clone(),
                attachments: message.attachments.clone(),
            })
    }

    pub fn send(&mut self, text: &str) -> Result<String, CodexServiceError> {
        self.send_prompt(ChatPrompt::text(text))
    }

    pub fn send_prompt(&mut self, mut prompt: ChatPrompt) -> Result<String, CodexServiceError> {
        prompt.text = prompt.text.trim().to_owned();
        if prompt.is_empty() {
            return Err(CodexServiceError::EmptyPrompt);
        }
        if self.is_busy() {
            return Err(CodexServiceError::Busy);
        }

        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id.to_string();
        self.last_prompt = Some(prompt.clone());
        self.messages.push(ChatMessage {
            id: format!("user-{id}"),
            role: MessageRole::User,
            text: prompt.text,
            attachments: prompt.attachments,
            state: MessageState::Complete,
        });
        self.active_message_id = Some(id.clone());
        self.ensure_assistant(&id);
        Ok(id)
    }

    pub fn apply(&mut self, event: &CodexEvent) {
        match event {
            CodexEvent::AssistantMessageStarted { message_id } => {
                if self.active_message_id.as_deref() == Some(message_id) {
                    self.start_assistant_part(message_id);
                }
            }
            CodexEvent::AssistantTextDelta { message_id, delta } => {
                if self.active_message_id.as_deref() == Some(message_id) {
                    let index = self.ensure_assistant(message_id);
                    self.messages[index].text.push_str(delta);
                }
            }
            CodexEvent::TurnCompleted { message_id } => {
                self.finish(message_id, MessageState::Complete);
            }
            CodexEvent::TurnInterrupted { message_id } => {
                self.finish(message_id, MessageState::Interrupted);
            }
            CodexEvent::Failure {
                message_id: Some(message_id),
                ..
            } => {
                self.finish(message_id, MessageState::Failed);
            }
            CodexEvent::ModeChanged { mode, folder } => {
                self.mode = *mode;
                self.agent_folder.clone_from(folder);
            }
            CodexEvent::ActivityUpsert(activity) => {
                if let Some(existing) = self
                    .activities
                    .iter_mut()
                    .find(|existing| existing.id == activity.id)
                {
                    existing.clone_from(activity);
                } else {
                    self.activities.push(activity.clone());
                }
            }
            CodexEvent::QuestionAsked(question) => self.question = Some(question.clone()),
            CodexEvent::QuestionCleared { request_id } => {
                if self.question.as_ref().map(|question| &question.request_id) == Some(request_id) {
                    self.question = None;
                }
            }
            _ => {}
        }
    }

    pub fn new_chat(&mut self) {
        self.messages.clear();
        self.active_message_id = None;
        self.last_prompt = None;
        self.mode = ChatMode::Chat;
        self.agent_folder = None;
        self.activities.clear();
        self.question = None;
    }

    fn ensure_assistant(&mut self, message_id: &str) -> usize {
        let prefix = format!("assistant-{message_id}");
        if let Some(index) = self.messages.iter().rposition(|message| {
            message.id == prefix || message.id.starts_with(&format!("{prefix}-"))
        }) {
            return index;
        }
        self.messages.push(ChatMessage {
            id: prefix,
            role: MessageRole::Assistant,
            text: String::new(),
            attachments: Vec::new(),
            state: MessageState::Streaming,
        });
        self.messages.len() - 1
    }

    fn start_assistant_part(&mut self, message_id: &str) -> usize {
        let current = self.ensure_assistant(message_id);
        if self.messages[current].text.is_empty() {
            return current;
        }
        self.messages[current].state = MessageState::Complete;
        let prefix = format!("assistant-{message_id}");
        let part = self
            .messages
            .iter()
            .filter(|message| message.id == prefix || message.id.starts_with(&format!("{prefix}-")))
            .count()
            .saturating_add(1);
        self.messages.push(ChatMessage {
            id: format!("{prefix}-{part}"),
            role: MessageRole::Assistant,
            text: String::new(),
            attachments: Vec::new(),
            state: MessageState::Streaming,
        });
        self.messages.len() - 1
    }

    fn finish(&mut self, message_id: &str, state: MessageState) {
        if self.active_message_id.as_deref() != Some(message_id) {
            return;
        }
        let index = self.ensure_assistant(message_id);
        if state == MessageState::Interrupted && self.messages[index].text.is_empty() {
            self.messages.remove(index);
        } else {
            self.messages[index].state = state;
        }
        self.active_message_id = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_stream_and_complete_one_message() {
        let mut conversation = Conversation::default();
        let id = conversation.send("Hello").expect("send");

        assert_eq!(conversation.messages().len(), 2);
        assert_eq!(conversation.messages()[1].role, MessageRole::Assistant);
        assert_eq!(conversation.messages()[1].state, MessageState::Streaming);
        assert!(conversation.messages()[1].text.is_empty());

        conversation.apply(&CodexEvent::AssistantTextDelta {
            message_id: id.clone(),
            delta: "Hi ".to_owned(),
        });
        conversation.apply(&CodexEvent::AssistantTextDelta {
            message_id: id.clone(),
            delta: "there".to_owned(),
        });
        conversation.apply(&CodexEvent::TurnCompleted { message_id: id });

        assert_eq!(conversation.messages()[1].text, "Hi there");
        assert_eq!(conversation.messages()[1].state, MessageState::Complete);
        assert!(!conversation.is_busy());
    }

    #[test]
    fn rejects_empty_and_concurrent_prompts() {
        let mut conversation = Conversation::default();
        assert_eq!(conversation.send("  "), Err(CodexServiceError::EmptyPrompt));
        conversation.send("first").expect("first send");
        assert_eq!(conversation.send("second"), Err(CodexServiceError::Busy));
    }

    #[test]
    fn accepts_an_image_only_prompt_and_preserves_it_for_retry() {
        let attachment = ChatAttachment {
            id: "attachment-1".to_owned(),
            path: PathBuf::from("/tmp/example.png"),
            mime_type: "image/png".to_owned(),
            width: 640,
            height: 480,
            byte_len: 123,
        };
        let prompt = ChatPrompt {
            text: String::new(),
            attachments: vec![attachment],
        };
        let mut conversation = Conversation::default();

        let id = conversation
            .send_prompt(prompt.clone())
            .expect("send image");

        assert_eq!(conversation.last_prompt(), Some(&prompt));
        assert_eq!(conversation.messages()[0].attachments, prompt.attachments);
        assert_eq!(
            conversation.prompt_for_assistant(&format!("assistant-{id}")),
            Some(prompt)
        );
    }

    #[test]
    fn new_chat_ignores_late_abandoned_events() {
        let mut conversation = Conversation::default();
        let old_id = conversation.send("old").expect("send");
        conversation.new_chat();
        conversation.apply(&CodexEvent::AssistantTextDelta {
            message_id: old_id,
            delta: "late".to_owned(),
        });
        assert!(conversation.messages().is_empty());
    }

    #[test]
    fn interrupt_removes_an_empty_assistant_placeholder() {
        let mut conversation = Conversation::default();
        let id = conversation.send("Hello").expect("send");

        conversation.apply(&CodexEvent::TurnInterrupted { message_id: id });

        assert_eq!(conversation.messages().len(), 1);
        assert_eq!(conversation.messages()[0].role, MessageRole::User);
        assert!(!conversation.is_busy());
    }

    #[test]
    fn interrupt_keeps_partial_assistant_text() {
        let mut conversation = Conversation::default();
        let id = conversation.send("Hello").expect("send");
        conversation.apply(&CodexEvent::AssistantTextDelta {
            message_id: id.clone(),
            delta: "Partial response".to_owned(),
        });

        conversation.apply(&CodexEvent::TurnInterrupted { message_id: id });

        assert_eq!(conversation.messages().len(), 2);
        assert_eq!(conversation.messages()[1].text, "Partial response");
        assert_eq!(conversation.messages()[1].state, MessageState::Interrupted);
        assert!(!conversation.is_busy());
    }

    #[test]
    fn resolves_the_prompt_for_each_assistant_response() {
        let mut conversation = Conversation::default();
        let first_id = conversation.send("first prompt").expect("first send");
        conversation.apply(&CodexEvent::TurnCompleted {
            message_id: first_id,
        });
        let second_id = conversation.send("second prompt").expect("second send");
        conversation.apply(&CodexEvent::TurnCompleted {
            message_id: second_id,
        });

        assert_eq!(
            conversation.prompt_for_assistant("assistant-1"),
            Some(ChatPrompt::text("first prompt"))
        );
        assert_eq!(
            conversation.prompt_for_assistant("assistant-2"),
            Some(ChatPrompt::text("second prompt"))
        );
        assert_eq!(conversation.prompt_for_assistant("missing"), None);
    }

    #[test]
    fn keeps_multiple_assistant_items_in_their_stream_order() {
        let mut conversation = Conversation::default();
        let id = conversation.send("work").expect("send");
        conversation.apply(&CodexEvent::AssistantTextDelta {
            message_id: id.clone(),
            delta: "Checking…".to_owned(),
        });
        conversation.apply(&CodexEvent::AssistantMessageStarted {
            message_id: id.clone(),
        });
        conversation.apply(&CodexEvent::AssistantTextDelta {
            message_id: id.clone(),
            delta: "Done.".to_owned(),
        });
        conversation.apply(&CodexEvent::TurnCompleted { message_id: id });

        assert_eq!(conversation.messages().len(), 3);
        assert_eq!(conversation.messages()[1].text, "Checking…");
        assert_eq!(conversation.messages()[1].state, MessageState::Complete);
        assert_eq!(conversation.messages()[2].text, "Done.");
        assert_eq!(conversation.messages()[2].state, MessageState::Complete);
    }

    #[test]
    fn agent_state_is_ordered_updatable_and_fully_ephemeral() {
        let mut conversation = Conversation::default();
        let folder = PathBuf::from("/tmp/project");
        conversation.apply(&CodexEvent::ModeChanged {
            mode: ChatMode::Agent,
            folder: Some(folder.clone()),
        });
        let activity = |id: &str, status: &str| AgentActivity {
            id: id.to_owned(),
            kind: AgentActivityKind::Command,
            title: id.to_owned(),
            detail: status.to_owned(),
            status: status.to_owned(),
            truncated: false,
        };
        conversation.apply(&CodexEvent::ActivityUpsert(activity("first", "inProgress")));
        conversation.apply(&CodexEvent::ActivityUpsert(activity("second", "completed")));
        conversation.apply(&CodexEvent::ActivityUpsert(activity("first", "completed")));
        conversation.apply(&CodexEvent::QuestionAsked(AgentQuestion {
            request_id: AgentRequestId::Number(7),
            fields: Vec::new(),
        }));

        assert_eq!(conversation.mode(), ChatMode::Agent);
        assert_eq!(conversation.agent_folder(), Some(folder.as_path()));
        assert_eq!(conversation.activities()[0].id, "first");
        assert_eq!(conversation.activities()[0].status, "completed");
        assert_eq!(conversation.activities()[1].id, "second");
        assert!(conversation.question().is_some());

        conversation.new_chat();
        assert_eq!(conversation.mode(), ChatMode::Chat);
        assert!(conversation.agent_folder().is_none());
        assert!(conversation.activities().is_empty());
        assert!(conversation.question().is_none());
    }
}
