//! UI-independent reducer for the single experimental conversation.

use crate::{CodexEvent, CodexServiceError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageState {
    Complete,
    Streaming,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatMessage {
    pub id: String,
    pub role: MessageRole,
    pub text: String,
    pub state: MessageState,
}

#[derive(Debug, Default)]
pub struct Conversation {
    messages: Vec<ChatMessage>,
    active_message_id: Option<String>,
    last_prompt: Option<String>,
    next_id: u64,
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
    pub fn last_prompt(&self) -> Option<&str> {
        self.last_prompt.as_deref()
    }

    pub fn send(&mut self, text: &str) -> Result<String, CodexServiceError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(CodexServiceError::EmptyPrompt);
        }
        if self.is_busy() {
            return Err(CodexServiceError::Busy);
        }

        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id.to_string();
        self.last_prompt = Some(text.to_owned());
        self.messages.push(ChatMessage {
            id: format!("user-{id}"),
            role: MessageRole::User,
            text: text.to_owned(),
            state: MessageState::Complete,
        });
        self.active_message_id = Some(id.clone());
        Ok(id)
    }

    pub fn apply(&mut self, event: &CodexEvent) {
        match event {
            CodexEvent::AssistantMessageStarted { message_id } => {
                if self.active_message_id.as_deref() == Some(message_id) {
                    self.ensure_assistant(message_id);
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
            _ => {}
        }
    }

    pub fn new_chat(&mut self) {
        self.messages.clear();
        self.active_message_id = None;
        self.last_prompt = None;
    }

    fn ensure_assistant(&mut self, message_id: &str) -> usize {
        let id = format!("assistant-{message_id}");
        if let Some(index) = self.messages.iter().position(|message| message.id == id) {
            return index;
        }
        self.messages.push(ChatMessage {
            id,
            role: MessageRole::Assistant,
            text: String::new(),
            state: MessageState::Streaming,
        });
        self.messages.len() - 1
    }

    fn finish(&mut self, message_id: &str, state: MessageState) {
        if self.active_message_id.as_deref() != Some(message_id) {
            return;
        }
        let index = self.ensure_assistant(message_id);
        self.messages[index].state = state;
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
}
