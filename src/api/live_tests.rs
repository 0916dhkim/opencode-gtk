//! Live checks against a real OpenCode 2.x server, driven by
//! `tests/v2/e2e.sh` against the isolated 2.0.8 harness (`tests/v2/`).
//!
//! Ignored by default. They go through the client's own `Api` code paths and
//! the real transcript reducer, and need:
//!
//! - `OCGTK_V2H_HARNESS=1`: set only by `tests/v2/e2e.sh`, so that a stray
//!   `--ignored` run can never drive a real (live) server;
//! - `OCGTK_LIVE_URL`: the harness server, e.g. `http://127.0.0.1:14096` (a
//!   loopback forward to the harness, since remote plain HTTP is refused;
//!   never 4096/4097, the ports of real servers on a developer host);
//! - `OCGTK_LIVE_PASSWORD_FILE`: file holding the Basic password;
//! - `OCGTK_LIVE_WORKSPACE`: the server-side project directory
//!   (default `/state/workspace`).
//!
//! The harness's mock provider picks its reply from a `[[scenario:…]]`
//! marker in the prompt (`tests/v2/README.md`).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;
use crate::model::Conversation;

const TERMINAL: &[&str] = &[
    "session.execution.succeeded",
    "session.execution.failed",
    "session.execution.interrupted",
];

struct Live {
    api: Api,
    workspace: String,
    events: Receiver<UiEvent>,
    /// Every `/api/event` event received so far, in arrival order.
    log: Vec<Value>,
    last_event: Instant,
    _alive: Arc<AtomicBool>,
}

impl Live {
    fn connect() -> Self {
        assert_eq!(
            std::env::var("OCGTK_V2H_HARNESS").as_deref(),
            Ok("1"),
            "refusing to run: OCGTK_V2H_HARNESS=1 marks the isolated harness (tests/v2/e2e.sh)"
        );
        let base_url = std::env::var("OCGTK_LIVE_URL").expect("OCGTK_LIVE_URL");
        let password_file =
            std::env::var("OCGTK_LIVE_PASSWORD_FILE").expect("OCGTK_LIVE_PASSWORD_FILE");
        let password = fs::read_to_string(&password_file)
            .expect("password file")
            .trim()
            .to_owned();
        let workspace =
            std::env::var("OCGTK_LIVE_WORKSPACE").unwrap_or_else(|_| "/state/workspace".into());
        let api = Api::new(ApiConfig {
            base_url,
            username: "opencode".into(),
            password: Some(password),
            cloudflare_access: None,
        })
        .expect("client config");
        let (sender, events) = async_channel::bounded(UI_EVENT_CAPACITY);
        let alive = Arc::new(AtomicBool::new(true));
        spawn_event_worker(api.clone(), sender, alive.clone());
        let live = Self {
            api,
            workspace,
            events,
            log: Vec::new(),
            last_event: Instant::now(),
            _alive: alive,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match live.events.try_recv() {
                Ok(UiEvent::Connection {
                    connected: true, ..
                }) => break,
                Ok(UiEvent::Connection { error, .. }) => {
                    panic!("event stream failed: {error:?}")
                }
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
                Err(error) => panic!("no server.connected: {error}"),
            }
        }
        live
    }

    fn pump(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                UiEvent::ServerEvent(envelope) => {
                    self.log.push(envelope.payload);
                    self.last_event = Instant::now();
                }
                UiEvent::Connection {
                    connected: false,
                    error,
                } => panic!("event stream dropped: {error:?}"),
                _ => {}
            }
        }
    }

    fn wait_for(
        &mut self,
        what: &str,
        timeout_s: u64,
        predicate: impl Fn(&Value) -> bool,
    ) -> Value {
        self.wait_for_since(0, what, timeout_s, predicate)
    }

    /// Log position for [`Live::wait_for_since`].
    fn mark(&mut self) -> usize {
        self.pump();
        self.log.len()
    }

    /// Like [`Live::wait_for`], ignoring events before `since`.
    fn wait_for_since(
        &mut self,
        since: usize,
        what: &str,
        timeout_s: u64,
        predicate: impl Fn(&Value) -> bool,
    ) -> Value {
        let deadline = Instant::now() + Duration::from_secs(timeout_s);
        loop {
            self.pump();
            if let Some(event) = self.log[since..].iter().find(|event| predicate(event)) {
                return event.clone();
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_quiet(&mut self, quiet: Duration, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.pump();
            if self.last_event.elapsed() >= quiet {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn terminal_count(&self, session_id: &str) -> usize {
        self.log
            .iter()
            .filter(|event| {
                TERMINAL.contains(&event["type"].as_str().unwrap_or_default())
                    && event["data"]["sessionID"] == session_id
            })
            .count()
    }

    fn wait_done(&mut self, session_id: &str, count: usize, timeout_s: u64) {
        let deadline = Instant::now() + Duration::from_secs(timeout_s);
        loop {
            self.pump();
            if self.terminal_count(session_id) >= count {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "session {session_id} did not finish {count} run(s)"
            );
            thread::sleep(Duration::from_millis(50));
        }
        self.wait_quiet(Duration::from_millis(1500), Duration::from_secs(20));
    }

    fn create(&self) -> Session {
        self.api
            .create_session(&self.workspace, None)
            .expect("create session")
    }

    fn prompt(&self, session_id: &str, text: &str, attachments: &[PathBuf]) -> String {
        let id = protocol::new_message_id();
        let accepted = self
            .api
            .send_prompt(session_id, id.clone(), text.into(), attachments)
            .expect("prompt");
        assert_eq!(accepted.id, id, "the prompt id is the inbox id");
        id
    }

    /// The live transcript: this session's events through the real reducer,
    /// routed by `sessionID` like the UI routes them.
    fn live(&self, session_id: &str) -> Conversation {
        let mut conversation = Conversation::default();
        for payload in &self.log {
            let event = protocol::Event::deserialize(payload).unwrap();
            let kind = protocol::decode_event(&event);
            if kind.session_id() == Some(session_id) {
                conversation.apply(&event, &kind);
            }
        }
        conversation
    }

    /// A reload through the client's paging: the newest page (with the inbox)
    /// replaces, older pages prepend, until the cursor runs out.
    fn reload(&self, session_id: &str) -> (Conversation, usize) {
        let mut conversation = Conversation::default();
        let first = self.api.load_messages(session_id, None).expect("history");
        conversation.replace_from_api(&first.messages, first.next_cursor.clone());
        if let Some(queued) = &first.queued {
            conversation.sync_queued(queued);
        }
        let mut pages = 1;
        while let Some(cursor) = conversation.next_cursor.clone() {
            let page = self
                .api
                .load_messages(session_id, Some(&cursor))
                .expect("older history");
            conversation.prepend_from_api(&page.messages, page.next_cursor);
            pages += 1;
            assert!(pages < 100, "history paging does not end");
        }
        (conversation, pages)
    }

    /// Raw history entries, oldest first, through the client's paging.
    fn entries(&self, session_id: &str) -> Vec<protocol::SessionMessage> {
        let mut pages = Vec::new();
        let mut cursor = None;
        loop {
            let page = self
                .api
                .load_messages(session_id, cursor.as_deref())
                .expect("history");
            cursor = page.next_cursor.clone();
            pages.push(page.messages);
            if cursor.is_none() {
                break;
            }
        }
        pages.into_iter().rev().flatten().collect()
    }

    /// Acceptance for one session: the streamed transcript equals a reload.
    fn assert_live_matches_history(&self, label: &str, session_id: &str) -> Vec<Value> {
        let live = self.live(session_id);
        let (reloaded, _) = self.reload(session_id);
        let live_rows = rows(&live);
        let reloaded_rows = rows(&reloaded);
        assert!(!reloaded_rows.is_empty(), "{label}: empty history");
        assert_eq!(
            live_rows, reloaded_rows,
            "{label}: live rows differ from history"
        );
        assert_eq!(shape(&live), shape(&reloaded), "{label}: message shape");
        assert_eq!(
            live.context_tokens(),
            reloaded.context_tokens(),
            "{label}: context usage"
        );
        eprintln!("PASS live==history {label} ({} rows)", live_rows.len());
        reloaded_rows
    }

    fn pending(&self) -> PendingSnapshot {
        let snapshot = self.api.load_pending(std::slice::from_ref(&self.workspace));
        assert!(snapshot.complete, "{:?}", snapshot.warnings);
        snapshot
    }

    fn assert_no_malformed_events(&self) {
        for payload in &self.log {
            let event = protocol::Event::deserialize(payload).expect("event envelope");
            if let protocol::EventKind::Malformed { type_, error } = protocol::decode_event(&event)
            {
                panic!("malformed {type_}: {error}");
            }
        }
    }
}

fn rows(conversation: &Conversation) -> Vec<Value> {
    conversation
        .transcript_rows()
        .iter()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect()
}

fn shape(conversation: &Conversation) -> Vec<(String, Vec<String>)> {
    conversation
        .messages
        .iter()
        .map(|message| (message.id.clone(), message.segment_keys()))
        .collect()
}

fn count_kind(rows: &[Value], role: &str, kind: &str) -> usize {
    rows.iter()
        .filter(|row| row["role"] == role && row["kind"] == kind)
        .count()
}

fn is_type(event: &Value, kind: &str, session_id: Option<&str>) -> bool {
    event["type"] == kind && session_id.is_none_or(|id| event["data"]["sessionID"] == id)
}

const PIXEL: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

/// One scenario after another on one event stream, like the app.
#[test]
#[ignore = "needs a live OpenCode 2.x server; run tests/v2/e2e.sh"]
fn live_server_end_to_end() {
    let mut live = Live::connect();
    let step = |name: &str| eprintln!("== {name}");

    step("bootstrap");
    let bootstrap = live
        .api
        .bootstrap(&[], std::slice::from_ref(&live.workspace))
        .expect("bootstrap");
    assert!(bootstrap.version.starts_with("2."), "{}", bootstrap.version);
    assert!(bootstrap.sessions_complete && bootstrap.statuses_complete);
    assert!(
        bootstrap.pending_covered.contains(&live.workspace),
        "{:?}",
        bootstrap.warnings
    );
    eprintln!("PASS bootstrap version {}", bootstrap.version);

    step("models");
    let deadline = Instant::now() + Duration::from_secs(30);
    let catalog = loop {
        let catalog = live.api.load_models(&live.workspace).expect("models");
        if !catalog.models.is_empty() {
            break catalog;
        }
        assert!(Instant::now() < deadline, "the model catalog stays empty");
        thread::sleep(Duration::from_millis(500));
    };
    let ids: Vec<String> = catalog
        .models
        .iter()
        .map(|model| format!("{}/{}", model.provider_id, model.model_id))
        .collect();
    assert!(ids.iter().any(|id| id == "mock/mock-model"), "{ids:?}");
    assert!(ids.iter().any(|id| id == "mock/mock-model-alt"), "{ids:?}");
    eprintln!("PASS models {ids:?}");

    step("create + rename");
    let session = live.create();
    assert_eq!(session.directory, live.workspace);
    let renamed = live
        .api
        .rename_session(&session.id, "Renamed by the live test")
        .expect("rename");
    assert_eq!(renamed.title, "Renamed by the live test");
    assert!(live.api.rename_session(&session.id, "   ").is_err());
    live.wait_for("session.renamed", 10, |event| {
        is_type(event, "session.renamed", Some(&session.id))
            && event["data"]["title"] == "Renamed by the live test"
    });
    eprintln!("PASS create + rename {}", session.id);

    step("text");
    live.prompt(&session.id, "Say hello. [[scenario:text]]", &[]);
    live.wait_done(&session.id, 1, 60);
    let rows = live.assert_live_matches_history("text", &session.id);
    assert_eq!(count_kind(&rows, "YOU", ""), 1);
    assert!(count_kind(&rows, "AGENT", "") >= 1);

    step("reasoning");
    let reasoning = live.create();
    live.prompt(&reasoning.id, "Think first. [[scenario:reasoning]]", &[]);
    live.wait_done(&reasoning.id, 1, 60);
    let rows = live.assert_live_matches_history("reasoning", &reasoning.id);
    assert!(count_kind(&rows, "AGENT", "reasoning") >= 1, "{rows:?}");

    step("tools");
    let tools = live.create();
    live.prompt(&tools.id, "Read two things. [[scenario:tools]]", &[]);
    live.wait_done(&tools.id, 1, 60);
    let rows = live.assert_live_matches_history("tools", &tools.id);
    assert!(count_kind(&rows, "AGENT", "tool") >= 2, "{rows:?}");

    step("error");
    let error = live.create();
    live.prompt(&error.id, "Fail please. [[scenario:error]]", &[]);
    live.wait_done(&error.id, 1, 60);
    let rows = live.assert_live_matches_history("error", &error.id);
    assert!(count_kind(&rows, "AGENT", "error") >= 1, "{rows:?}");

    step("retry");
    let retry = live.create();
    live.prompt(&retry.id, "Fail once. [[scenario:retry]]", &[]);
    live.wait_done(&retry.id, 1, 90);
    assert!(live.log.iter().any(|event| is_type(
        event,
        "session.retry.scheduled",
        Some(&retry.id)
    )));
    let rows = live.assert_live_matches_history("retry", &retry.id);
    assert!(count_kind(&rows, "AGENT", "") >= 1, "{rows:?}");

    step("attachment");
    let attachment = live.create();
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("pixel.png");
    {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(PIXEL)
            .unwrap();
        fs::write(&image, bytes).unwrap();
    }
    live.prompt(
        &attachment.id,
        "Describe the attached image. [[scenario:text]]",
        &[image],
    );
    live.wait_done(&attachment.id, 1, 60);
    let rows = live.assert_live_matches_history("attachment", &attachment.id);
    let user = rows.iter().find(|row| row["role"] == "YOU").unwrap();
    let images = user["images"].as_array().expect("user row images");
    assert_eq!(images.len(), 1, "{user}");
    assert!(images[0]
        .as_str()
        .is_some_and(|uri| uri.starts_with("data:image/png;base64,")));
    let stored = live.entries(&attachment.id);
    let protocol::SessionMessage::User(message) = &stored[0] else {
        panic!("first entry is not the user prompt: {:?}", stored[0]);
    };
    assert_eq!(message.files[0].mime, "image/png");
    assert_eq!(message.files[0].name.as_deref(), Some("pixel.png"));
    eprintln!("PASS attachment round-trip");

    step("attachment over 20 MiB is refused before sending");
    let big = directory.path().join("big.bin");
    File::create(&big)
        .unwrap()
        .set_len(MAX_ATTACHMENT_BYTES + 1)
        .unwrap();
    let refused = live
        .api
        .send_prompt(
            &attachment.id,
            protocol::new_message_id(),
            "too big".into(),
            &[big],
        )
        .unwrap_err();
    assert!(format_error(refused).contains("larger than 20 MiB"));
    eprintln!("PASS 20 MiB rejection");

    step("model switch");
    let switched = live.create();
    let alt = protocol::ModelRef {
        id: "mock-model-alt".into(),
        provider_id: "mock".into(),
        variant: None,
    };
    live.api.select_model(&switched.id, &alt).expect("switch");
    live.wait_for("session.model.selected", 10, |event| {
        is_type(event, "session.model.selected", Some(&switched.id))
    });
    let reloaded: protocol::SessionResponse = live
        .api
        .get(&protocol::session_path(&switched.id), &[])
        .unwrap();
    let saved = Session::from_info(&reloaded.data);
    assert_eq!(
        saved.model.as_ref().map(|model| model.id.as_str()),
        Some("mock-model-alt")
    );
    live.prompt(&switched.id, "Use the alt model. [[scenario:text]]", &[]);
    live.wait_done(&switched.id, 1, 60);
    live.assert_live_matches_history("model switch", &switched.id);
    let assistant_model = live
        .entries(&switched.id)
        .into_iter()
        .find_map(|entry| match entry {
            protocol::SessionMessage::Assistant(message) => message.model,
            _ => None,
        })
        .expect("assistant entry with a model");
    assert_eq!(assistant_model.id, "mock-model-alt");
    eprintln!("PASS model switch persists and is used");

    step("permission reply once");
    let permission = live.create();
    live.prompt(
        &permission.id,
        "Run a command. [[scenario:permission]]",
        &[],
    );
    let asked = live.wait_for("permission.asked", 30, |event| {
        is_type(event, "permission.asked", Some(&permission.id))
    });
    let request_id = asked["data"]["id"].as_str().unwrap().to_owned();
    let pending = live.pending();
    assert!(
        pending.requests.iter().any(|request| matches!(
            request,
            PendingRequest::Permission { request, .. } if request.id == request_id
        )),
        "{:?}",
        pending.requests
    );
    assert_eq!(
        live.api
            .reply_permission(
                &permission.id,
                &request_id,
                protocol::PermissionDecision::Once
            )
            .expect("reply"),
        Settled::Done
    );
    assert_eq!(
        live.api
            .reply_permission(
                &permission.id,
                &request_id,
                protocol::PermissionDecision::Once
            )
            .expect("second reply"),
        Settled::AlreadyResolved
    );
    live.wait_done(&permission.id, 1, 60);
    let rows = live.assert_live_matches_history("permission", &permission.id);
    assert!(count_kind(&rows, "AGENT", "tool") >= 1, "{rows:?}");
    assert!(!live
        .pending()
        .requests
        .iter()
        .any(|request| request.id() == request_id));

    step("subagent");
    let parent = live.create();
    live.prompt(&parent.id, "Delegate this. [[scenario:subagent]]", &[]);
    live.wait_done(&parent.id, 2, 90);
    let rows = live.assert_live_matches_history("subagent", &parent.id);
    assert!(count_kind(&rows, "AGENT", "tool") >= 1, "{rows:?}");
    let child = live
        .log
        .iter()
        .find(|event| {
            event["type"] == "session.created" && event["data"]["parentID"] == parent.id.as_str()
        })
        .and_then(|event| event["data"]["sessionID"].as_str())
        .expect("child session.created")
        .to_owned();
    live.assert_live_matches_history("subagent child", &child);

    step("child-session permission");
    let delegating = live.create();
    let since = live.mark();
    live.prompt(
        &delegating.id,
        "Delegate a command. [[scenario:subagent-permission]]",
        &[],
    );
    let asked = live.wait_for_since(since, "child permission.asked", 30, |event| {
        event["type"] == "permission.asked" && event["data"]["sessionID"] != delegating.id.as_str()
    });
    let child_id = asked["data"]["sessionID"].as_str().unwrap().to_owned();
    let request_id = asked["data"]["id"].as_str().unwrap().to_owned();
    assert!(live.pending().requests.iter().any(|request| matches!(
        request,
        PendingRequest::Permission { request, .. }
            if request.id == request_id && request.session_id == child_id
    )));
    assert_eq!(
        live.api
            .reply_permission(&child_id, &request_id, protocol::PermissionDecision::Once)
            .expect("child reply"),
        Settled::Done
    );
    live.wait_done(&delegating.id, 1, 90);
    live.assert_live_matches_history("child permission parent", &delegating.id);
    live.assert_live_matches_history("child permission child", &child_id);

    step("slow + queued prompt + interrupt");
    let slow = live.create();
    live.prompt(&slow.id, "Stream slowly. [[scenario:slow]]", &[]);
    live.wait_for("slow text delta", 30, |event| {
        is_type(event, "session.text.delta", Some(&slow.id))
    });
    let follow_up = live.prompt(&slow.id, "Sent while busy. [[scenario:text]]", &[]);
    thread::sleep(Duration::from_millis(800));
    assert!(live.api.abort(&slow.id).expect("interrupt"), "was running");
    live.wait_for("execution.interrupted", 30, |event| {
        is_type(event, "session.execution.interrupted", Some(&slow.id))
    });
    live.wait_quiet(Duration::from_secs(3), Duration::from_secs(60));
    live.assert_live_matches_history("interrupt", &slow.id);
    let (reloaded, _) = live.reload(&slow.id);
    assert!(
        reloaded.has_user_message(&follow_up),
        "the follow-up survives the interrupt, delivered or queued"
    );
    // Once nothing runs, an interrupt reports that it stopped nothing.
    if !live
        .api
        .load_statuses()
        .expect("active")
        .contains_key(&slow.id)
    {
        assert!(!live.api.abort(&slow.id).expect("idle interrupt"));
        eprintln!("PASS idle interrupt reports false");
    }

    step("form notice data + cancel");
    let form_owner = live.create();
    let created: Value = live
        .api
        .send_json(
            Method::POST,
            &format!("{}/form", protocol::session_path(&form_owner.id)),
            &json!({
                "title": "Live test form",
                "fields": [{ "key": "name", "type": "string", "title": "Name" }]
            }),
        )
        .expect("create form");
    let form_id = created["data"]["id"].as_str().unwrap().to_owned();
    let event = live.wait_for("form.created", 10, |event| {
        event["type"] == "form.created" && event["data"]["form"]["id"] == form_id.as_str()
    });
    let decoded = protocol::Event::deserialize(&event).unwrap();
    let change =
        crate::pending::pending_change(&protocol::decode_event(&decoded), decoded.directory());
    assert!(
        matches!(&change, Some(crate::pending::PendingChange::Form(form)) if form.form.id == form_id && form.form.title == "Live test form"),
        "{change:?}"
    );
    let pending = live.pending();
    let form = pending
        .requests
        .iter()
        .find_map(|request| match request {
            PendingRequest::Form(form) if form.form.id == form_id => Some(form.clone()),
            _ => None,
        })
        .expect("the form is pending");
    assert_eq!(
        live.api
            .cancel_form(&form.form.session_id, &form_id, form.directory.as_deref())
            .expect("cancel"),
        Settled::Done
    );
    assert_eq!(
        live.api
            .cancel_form(&form.form.session_id, &form_id, form.directory.as_deref())
            .expect("cancel again"),
        Settled::AlreadyResolved
    );
    live.wait_for("form.cancelled", 10, |event| {
        event["type"] == "form.cancelled" && event["data"]["id"] == form_id.as_str()
    });
    assert!(!live
        .pending()
        .requests
        .iter()
        .any(|request| request.id() == form_id));
    eprintln!("PASS form pending -> cancel");

    step("history paging");
    let long = live.create();
    let turns = 30;
    for turn in 1..=turns {
        live.prompt(&long.id, &format!("Turn {turn}. [[scenario:text]]"), &[]);
        live.wait_done(&long.id, turn, 60);
    }
    let (reloaded, pages) = live.reload(&long.id);
    assert!(pages >= 2, "{pages} page(s)");
    let prompts = reloaded
        .transcript_rows()
        .iter()
        .filter(|row| row.contains("\"role\":\"YOU\""))
        .count();
    assert_eq!(prompts, turns);
    live.assert_live_matches_history("history paging", &long.id);
    eprintln!("PASS history paging over {pages} pages");

    step("bootstrap again");
    let again = live
        .api
        .bootstrap(&[], std::slice::from_ref(&live.workspace))
        .expect("bootstrap");
    for id in [&session.id, &long.id, &parent.id] {
        assert!(
            again.sessions.iter().any(|session| &session.id == id),
            "{id}"
        );
    }
    assert!(
        !again.sessions.iter().any(|session| session.id == child),
        "child sessions are not roots"
    );
    assert!(again
        .sessions
        .iter()
        .any(|listed| listed.id == session.id && listed.title == "Renamed by the live test"));

    live.assert_no_malformed_events();
    eprintln!("PASS {} events decoded without errors", live.log.len());
}
