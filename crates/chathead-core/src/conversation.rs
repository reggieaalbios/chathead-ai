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

    #[must_use]
    pub fn prompt_for_assistant(&self, assistant_message_id: &str) -> Option<&str> {
        let assistant_index = self
            .messages
            .iter()
            .position(|message| message.id == assistant_message_id)?;
        self.messages[..assistant_index]
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
            .map(|message| message.text.as_str())
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
        self.ensure_assistant(&id);
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
            Some("first prompt")
        );
        assert_eq!(
            conversation.prompt_for_assistant("assistant-2"),
            Some("second prompt")
        );
        assert_eq!(conversation.prompt_for_assistant("missing"), None);
    }
}
