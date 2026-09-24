use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use crate::{
    api::{Bootstrap, Command, MessagePage, UiEvent},
    model::{ModelCatalog, ModelOption, ModelSelection, Project, RunStatus, Session},
    persist::{PersistedTab, ServerState},
    protocol,
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
        unread: HashSet::new(),
        busy: HashSet::new(),
    }
}

pub struct State {
    sessions: Vec<Session>,
    /// v2 message-list entries per session, oldest first.
    messages: HashMap<String, Vec<protocol::SessionMessage>>,
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
            Command::LoadMessages { session_id, cursor } => UiEvent::MessagesLoaded {
                result: Ok(self.message_page(&session_id, cursor.as_deref())),
                session_id,
                cursor,
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
            projects: vec![Project::from_info(&decode(json!({
                "id": "prj_preview",
                "canonical": DIRECTORY,
                "name": "opencode-gtk",
                "sandboxes": []
            })))],
            statuses,
            statuses_complete: true,
            pending: Vec::new(),
            // Matches the real client until pending recovery is ported.
            pending_complete: false,
            retry_needed: false,
            warnings: Vec::new(),
        }
    }

    /// History as the client presents it: the whole canned transcript is one
    /// chronological page, so there is never an older page.
    fn message_page(&self, session_id: &str, cursor: Option<&str>) -> MessagePage {
        if cursor.is_some() {
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
        let created = CREATED + self.next_id * 1_000;
        let session = session_info(&id, &directory, title.as_deref(), created, created);
        self.messages.insert(id, Vec::new());
        self.sessions.insert(0, session.clone());
        session
    }

    fn rename_session(&mut self, session_id: &str, title: String) -> Result<Session, String> {
        let title = title.trim().to_owned();
        if title.is_empty() {
            return Err("session title cannot be blank".into());
        }
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
        let message = entry(json!({
            "id": id,
            "type": "user",
            "time": { "created": CREATED + self.next_id * 1_000 },
            "text": text
        }));
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
                context_limit: Some(200_000),
            },
            ModelOption {
                provider_id: "anthropic".into(),
                model_id: "claude-sonnet-4.6".into(),
                label: "Anthropic / Claude Sonnet 4.6".into(),
                variants: vec!["medium".into(), "high".into()],
                supports_attachments: true,
                context_limit: Some(200_000),
            },
        ],
        preferred: Some(ModelSelection {
            provider_id: "openai".into(),
            model_id: "gpt-5.6".into(),
            variant: Some("medium".into()),
        }),
    }
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("canned preview data matches the v2 protocol")
}

/// A canned v2 `SessionInfo`, mapped the same way the real client maps it.
fn session_info(
    id: &str,
    directory: &str,
    title: Option<&str>,
    created: u64,
    updated: u64,
) -> Session {
    Session::from_info(&decode::<protocol::SessionInfo>(json!({
        "id": id,
        "projectID": "prj_preview",
        "agent": "build",
        "model": { "id": "gpt-5.6", "providerID": "openai", "variant": "medium" },
        "cost": 0,
        "tokens": { "input": 0, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
        "time": { "created": created, "updated": updated },
        "title": title,
        "location": { "directory": directory }
    })))
}

fn active_session() -> Session {
    session_info(
        ACTIVE_ID,
        DIRECTORY,
        Some("Fix the attach clip padding"),
        CREATED,
        CREATED + 90_000,
    )
}

fn other_session() -> Session {
    session_info(
        OTHER_ID,
        DIRECTORY,
        Some("SSH tunnel notes"),
        CREATED - 86_400_000,
        CREATED - 3_600_000,
    )
}

fn entry(value: Value) -> protocol::SessionMessage {
    let message = protocol::SessionMessage::from_value(value);
    assert!(
        !matches!(message, protocol::SessionMessage::Unknown(_)),
        "canned preview entry matches the v2 protocol: {message:?}"
    );
    message
}

fn user_text(id: &str, created: u64, text: &str) -> protocol::SessionMessage {
    entry(json!({ "id": id, "type": "user", "time": { "created": created }, "text": text }))
}

fn idle(id: &str, created: u64) -> protocol::SessionMessage {
    entry(
        json!({ "id": id, "type": "idle", "time": { "created": created }, "outcome": "succeeded" }),
    )
}

fn active_messages() -> Vec<protocol::SessionMessage> {
    vec![
        user_text(
            "msg_user",
            CREATED,
            "The paperclip is crowding the attach button. Match send's padding.",
        ),
        entry(json!({
            "id": "msg_assistant",
            "type": "assistant",
            "time": { "created": CREATED + 30_000, "completed": CREATED + 50_000 },
            "agent": "build",
            "model": { "id": "gpt-5.6", "providerID": "openai" },
            "content": [
                {
                    "type": "reasoning",
                    "text": "The clip is a tall outline, so it reads larger than the paper plane at the same pixel size.",
                    "time": { "created": CREATED + 31_000, "completed": CREATED + 33_000 }
                },
                {
                    "type": "text",
                    "text": "# Padding\n\nDraw the clip at **22px**, same as send.\n\n- Inner wire stays visible\n- Composer actions stay `34×32`\n\n```rust\npaperclip_icon(COMPOSER_ICON_PX)\n```"
                },
                {
                    "type": "tool",
                    "id": "call_preview_shell",
                    "name": "shell",
                    "state": {
                        "status": "completed",
                        "input": { "command": "cargo test composer", "description": "Run composer tests" },
                        "content": [{ "type": "text", "text": "test result: ok. 12 passed" }]
                    },
                    "time": { "created": CREATED + 45_000, "ran": CREATED + 45_100, "completed": CREATED + 48_000 }
                }
            ],
            "finish": "stop",
            "tokens": { "input": 12400, "output": 800, "reasoning": 200, "cache": { "read": 0, "write": 0 } }
        })),
        idle("msg_idle", CREATED + 50_000),
    ]
}

fn other_messages() -> Vec<protocol::SessionMessage> {
    vec![
        user_text(
            "msg_other_user",
            CREATED - 86_400_000,
            "How do I reach the remote serve over SSH?",
        ),
        entry(json!({
            "id": "msg_other_assistant",
            "type": "assistant",
            "time": { "created": CREATED - 86_370_000 },
            "agent": "build",
            "content": [{
                "type": "text",
                "text": "Tunnel loopback: `ssh -N -L 4096:127.0.0.1:4096 host`, then connect to `http://127.0.0.1:4096`."
            }]
        })),
        entry(json!({
            "id": "msg_other_synthetic",
            "type": "synthetic",
            "time": { "created": CREATED - 86_340_000 },
            "text": "The background subagent finished: port 4096 is reachable through the tunnel.",
            "description": "Check the tunnel",
            "metadata": { "source": "subagent", "state": "completed" }
        })),
        entry(json!({
            "id": "msg_other_error",
            "type": "assistant",
            "time": { "created": CREATED - 86_300_000 },
            "agent": "build",
            "content": [],
            "finish": "error",
            "error": { "type": "provider.api", "message": "AI_APICallError: Not Found (404)", "status": 404 }
        })),
    ]
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

    fn render(messages: &[protocol::SessionMessage]) -> Vec<Value> {
        let mut conversation = Conversation::default();
        conversation.replace_from_api(messages, None);
        conversation
            .transcript_rows()
            .iter()
            .map(|row| serde_json::from_str(row).unwrap())
            .collect()
    }

    #[test]
    fn canned_messages_render_as_transcript_rows() {
        let rows = render(&active_messages());
        assert_eq!(rows.len(), 4, "the idle entry renders nothing");
        assert_eq!(rows[0]["role"], "YOU");
        assert_eq!(rows[1]["kind"], "reasoning");
        assert_eq!(rows[1]["time"], CREATED + 31_000);
        assert!(rows[2]["body"].as_str().unwrap().contains("22px"));
        assert_eq!(rows[3]["kind"], "tool");
        assert_eq!(rows[3]["body"], "shell · completed — cargo test composer");
        assert_eq!(rows[3]["time"], CREATED + 45_000);

        let rows = render(&other_messages());
        let kinds: Vec<_> = rows
            .iter()
            .map(|row| (row["role"].as_str().unwrap(), row["kind"].as_str().unwrap()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("YOU", ""),
                ("AGENT", ""),
                ("AGENT", ""),
                ("AGENT", "error")
            ]
        );
        assert!(rows[2]["body"]
            .as_str()
            .unwrap()
            .starts_with("The background subagent"));
        assert_eq!(rows[3]["body"], "AI_APICallError: Not Found (404)");
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
        let untitled = state.create_session(DIRECTORY.into(), None);
        assert_eq!(untitled.title, "Untitled session");
    }

    #[test]
    fn blank_rename_is_refused() {
        let mut state = State::new();
        assert!(state.rename_session(ACTIVE_ID, "  ".into()).is_err());
        assert_eq!(
            state
                .rename_session(ACTIVE_ID, " Renamed ".into())
                .unwrap()
                .title,
            "Renamed"
        );
    }
}
