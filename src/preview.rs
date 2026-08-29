use std::collections::HashMap;

use serde_json::{json, Value};

use crate::{
    api::{Bootstrap, Command, MessagePage, UiEvent},
    model::{
        ModelCatalog, ModelOption, ModelSelection, Project, RunStatus, Session, SessionModel,
        SessionTime,
    },
    persist::{PersistedTab, ServerState},
};

pub const SERVER_KEY: &str = "preview://opencode-gtk";

const DIRECTORY: &str = "/repo";
const ACTIVE_ID: &str = "ses_preview";
const OTHER_ID: &str = "ses_other";
const CREATED: u64 = 1_704_067_200_000;

pub fn server_state() -> ServerState {
    ServerState {
        tabs: vec![
            PersistedTab {
                id: ACTIVE_ID.into(),
                directory: DIRECTORY.into(),
                title: "Fix the attach clip padding".into(),
            },
            PersistedTab {
                id: OTHER_ID.into(),
                directory: DIRECTORY.into(),
                title: "SSH tunnel notes".into(),
            },
        ],
        active: Some(ACTIVE_ID.into()),
        selections: HashMap::new(),
    }
}

pub struct State {
    sessions: Vec<Session>,
    messages: HashMap<String, Vec<Value>>,
    next_id: u64,
}

impl State {
    pub fn new() -> Self {
        let mut messages = HashMap::new();
        messages.insert(ACTIVE_ID.to_owned(), active_messages());
        messages.insert(OTHER_ID.to_owned(), other_messages());
        Self {
            sessions: vec![active_session(), other_session()],
            messages,
            next_id: 1,
        }
    }

    pub fn handle(&mut self, command: Command) -> UiEvent {
        match command {
            Command::Bootstrap => UiEvent::Bootstrap(Ok(self.bootstrap())),
            Command::LoadMessages {
                session_id, before, ..
            } => UiEvent::MessagesLoaded {
                result: Ok(self.message_page(&session_id, before.as_deref())),
                session_id,
                before,
            },
            Command::LoadModels { directory } => UiEvent::ModelsLoaded {
                directory,
                result: Ok(catalog()),
            },
            Command::CreateSession {
                request_id,
                directory,
                title,
            } => {
                let session = self.create_session(directory, title);
                UiEvent::SessionCreated {
                    request_id,
                    result: Ok(session),
                }
            }
            Command::RenameSession {
                request_id,
                session_id,
                title,
                ..
            } => {
                let result = self.rename_session(&session_id, title);
                UiEvent::SessionRenamed {
                    request_id,
                    session_id,
                    result,
                }
            }
            Command::SendPrompt {
                request_id,
                session_id,
                text,
                ..
            } => {
                self.append_user_message(&session_id, text);
                UiEvent::PromptAccepted {
                    request_id,
                    session_id,
                    result: Ok(()),
                }
            }
            Command::Abort { session_id, .. } => UiEvent::Aborted {
                session_id,
                result: Ok(()),
            },
            Command::ReplyPermission { request_id, .. }
            | Command::ReplyQuestion { request_id, .. }
            | Command::RejectQuestion { request_id, .. } => UiEvent::ActionFinished {
                request_id,
                result: Ok(()),
            },
        }
    }

    fn bootstrap(&self) -> Bootstrap {
        let statuses = self
            .sessions
            .iter()
            .map(|session| (session.id.clone(), RunStatus::Idle))
            .collect();
        Bootstrap {
            version: "preview".into(),
            sessions: self.sessions.clone(),
            sessions_complete: true,
            projects: vec![Project {
                worktree: DIRECTORY.into(),
                name: Some("opencode-gtk".into()),
            }],
            statuses,
            statuses_complete: true,
            pending: Vec::new(),
            pending_complete: true,
            retry_needed: false,
            warnings: Vec::new(),
        }
    }

    fn message_page(&self, session_id: &str, before: Option<&str>) -> MessagePage {
        if before.is_some() {
            return MessagePage {
                messages: Vec::new(),
                next_cursor: None,
            };
        }
        MessagePage {
            messages: self.messages.get(session_id).cloned().unwrap_or_default(),
            next_cursor: None,
        }
    }

    fn create_session(&mut self, directory: String, title: Option<String>) -> Session {
        let id = format!("ses_new_{}", self.next_id);
        self.next_id += 1;
        let session = Session {
            id: id.clone(),
            directory,
            title: title.unwrap_or_else(|| "New session".into()),
            time: SessionTime {
                created: CREATED + self.next_id * 1_000,
                updated: CREATED + self.next_id * 1_000,
                archived: None,
            },
            parent_id: None,
            agent: None,
            model: Some(preferred_model()),
        };
        self.messages.insert(id, Vec::new());
        self.sessions.insert(0, session.clone());
        session
    }

    fn rename_session(&mut self, session_id: &str, title: String) -> Result<Session, String> {
        let session = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .ok_or_else(|| format!("unknown session {session_id}"))?;
        session.title = title;
        session.time.updated = CREATED + 60_000;
        Ok(session.clone())
    }

    fn append_user_message(&mut self, session_id: &str, text: String) {
        let id = format!("msg_user_{}", self.next_id);
        self.next_id += 1;
        let message = text_message(
            &id,
            session_id,
            "user",
            CREATED + self.next_id * 1_000,
            &text,
        );
        self.messages
            .entry(session_id.to_owned())
            .or_default()
            .push(message);
    }
}

fn catalog() -> ModelCatalog {
    ModelCatalog {
        models: vec![
            ModelOption {
                provider_id: "openai".into(),
                model_id: "gpt-5.6".into(),
                label: "OpenAI / GPT-5.6".into(),
                variants: vec!["low".into(), "medium".into(), "high".into()],
                supports_attachments: true,
            },
            ModelOption {
                provider_id: "anthropic".into(),
                model_id: "claude-sonnet-4.6".into(),
                label: "Anthropic / Claude Sonnet 4.6".into(),
                variants: vec!["medium".into(), "high".into()],
                supports_attachments: true,
            },
        ],
        preferred: Some(ModelSelection {
            provider_id: "openai".into(),
            model_id: "gpt-5.6".into(),
            variant: Some("medium".into()),
        }),
    }
}

fn preferred_model() -> SessionModel {
    SessionModel {
        id: "gpt-5.6".into(),
        provider_id: "openai".into(),
        variant: Some("medium".into()),
    }
}

fn active_session() -> Session {
    Session {
        id: ACTIVE_ID.into(),
        directory: DIRECTORY.into(),
        title: "Fix the attach clip padding".into(),
        time: SessionTime {
            created: CREATED,
            updated: CREATED + 90_000,
            archived: None,
        },
        parent_id: None,
        agent: None,
        model: Some(preferred_model()),
    }
}

fn other_session() -> Session {
    Session {
        id: OTHER_ID.into(),
        directory: DIRECTORY.into(),
        title: "SSH tunnel notes".into(),
        time: SessionTime {
            created: CREATED - 86_400_000,
            updated: CREATED - 3_600_000,
            archived: None,
        },
        parent_id: None,
        agent: None,
        model: Some(preferred_model()),
    }
}

fn active_messages() -> Vec<Value> {
    vec![
        text_message(
            "msg_user",
            ACTIVE_ID,
            "user",
            CREATED,
            "The paperclip is crowding the attach button. Match send's padding.",
        ),
        json!({
            "info": {
                "id": "msg_assistant",
                "sessionID": ACTIVE_ID,
                "role": "assistant",
                "time": { "created": CREATED + 30_000 }
            },
            "parts": [
                {
                    "id": "part_reason",
                    "messageID": "msg_assistant",
                    "sessionID": ACTIVE_ID,
                    "type": "reasoning",
                    "text": "The clip is a tall outline, so it reads larger than the paper plane at the same pixel size."
                },
                {
                    "id": "part_text",
                    "messageID": "msg_assistant",
                    "sessionID": ACTIVE_ID,
                    "type": "text",
                    "text": "# Padding\n\nDraw the clip at **22px**, same as send.\n\n- Inner wire stays visible\n- Composer actions stay `34×32`\n\n```rust\npaperclip_icon(COMPOSER_ICON_PX)\n```"
                },
                {
                    "id": "part_tool",
                    "messageID": "msg_assistant",
                    "sessionID": ACTIVE_ID,
                    "type": "tool",
                    "tool": "bash",
                    "state": {
                        "status": "completed",
                        "time": { "start": CREATED + 45_000 }
                    }
                }
            ]
        }),
    ]
}

fn other_messages() -> Vec<Value> {
    vec![
        text_message(
            "msg_other_user",
            OTHER_ID,
            "user",
            CREATED - 86_400_000,
            "How do I reach the remote serve over SSH?",
        ),
        text_message(
            "msg_other_assistant",
            OTHER_ID,
            "assistant",
            CREATED - 86_370_000,
            "Tunnel loopback: `ssh -N -L 4096:127.0.0.1:4096 host`, then connect to `http://127.0.0.1:4096`.",
        ),
    ]
}

fn text_message(id: &str, session_id: &str, role: &str, created: u64, text: &str) -> Value {
    json!({
        "info": {
            "id": id,
            "sessionID": session_id,
            "role": role,
            "time": { "created": created }
        },
        "parts": [{
            "id": format!("part_{id}"),
            "messageID": id,
            "sessionID": session_id,
            "type": "text",
            "text": text
        }]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Conversation;

    #[test]
    fn bootstrap_opens_a_representative_session() {
        let bootstrap = State::new().bootstrap();
        assert_eq!(bootstrap.version, "preview");
        assert_eq!(bootstrap.sessions[0].id, ACTIVE_ID);
        assert!(bootstrap.sessions_complete);
        assert!(catalog().models[0].supports_attachments);
    }

    #[test]
    fn canned_messages_render_as_transcript_rows() {
        let mut conversation = Conversation::default();
        conversation.replace_from_api(&active_messages(), None);
        let rows: Vec<Value> = conversation
            .transcript_rows()
            .iter()
            .map(|row| serde_json::from_str(row).unwrap())
            .collect();
        assert_eq!(rows[0]["role"], "YOU");
        assert_eq!(rows[1]["kind"], "reasoning");
        assert!(rows[2]["body"].as_str().unwrap().contains("22px"));
        assert_eq!(rows[3]["kind"], "tool");
    }

    #[test]
    fn create_session_returns_a_new_tab() {
        let mut state = State::new();
        let event = state.handle(Command::CreateSession {
            request_id: 1,
            directory: DIRECTORY.into(),
            title: Some("Scratch".into()),
        });
        match event {
            UiEvent::SessionCreated {
                result: Ok(session),
                ..
            } => {
                assert_eq!(session.title, "Scratch");
                assert_eq!(session.directory, DIRECTORY);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
