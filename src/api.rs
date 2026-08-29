use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Read},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use async_channel::{Receiver, Sender};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use reqwest::{
    blocking::{Client, RequestBuilder, Response},
    header::HeaderValue,
    Method, StatusCode, Url,
};
use serde_json::{json, Value};

use crate::{
    credentials::CloudflareAccessCredentials,
    model::{ModelCatalog, ModelSelection, Project, RunStatus, Session},
};

const MESSAGE_PAGE_SIZE: usize = 80;
const SESSION_LIST_LIMIT: usize = 100_000;
const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;
const MAX_TOTAL_ATTACHMENT_BYTES: u64 = 40 * 1024 * 1024;
const UI_EVENT_CAPACITY: usize = 4_096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiConfig {
    pub base_url: String,
    pub username: String,
    pub password: Option<String>,
    pub cloudflare_access: Option<CloudflareAccessCredentials>,
}

#[derive(Debug)]
pub enum Command {
    Bootstrap,
    LoadMessages {
        session_id: String,
        directory: String,
        before: Option<String>,
    },
    LoadModels {
        directory: String,
    },
    CreateSession {
        request_id: u64,
        directory: String,
        title: Option<String>,
    },
    RenameSession {
        request_id: u64,
        session_id: String,
        directory: String,
        title: String,
    },
    SendPrompt {
        request_id: u64,
        session_id: String,
        directory: String,
        text: String,
        selection: ModelSelection,
        agent: Option<String>,
        attachments: Vec<PathBuf>,
    },
    Abort {
        session_id: String,
        directory: String,
    },
    ReplyPermission {
        request_id: String,
        directory: String,
        reply: String,
    },
    ReplyQuestion {
        request_id: String,
        directory: String,
        answers: Vec<Vec<String>>,
    },
    RejectQuestion {
        request_id: String,
        directory: String,
    },
}

#[derive(Debug)]
pub struct Bootstrap {
    pub version: String,
    pub sessions: Vec<Session>,
    pub sessions_complete: bool,
    pub projects: Vec<Project>,
    pub statuses: HashMap<String, RunStatus>,
    pub statuses_complete: bool,
    pub pending: Vec<ServerEnvelope>,
    pub pending_complete: bool,
    pub retry_needed: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct MessagePage {
    pub messages: Vec<Value>,
    pub next_cursor: Option<String>,
}

#[derive(Debug)]
pub struct ServerEnvelope {
    pub directory: Option<String>,
    pub payload: Value,
}

#[derive(Debug)]
pub enum UiEvent {
    Connection {
        connected: bool,
        error: Option<String>,
    },
    Bootstrap(Result<Bootstrap, String>),
    MessagesLoaded {
        session_id: String,
        before: Option<String>,
        result: Result<MessagePage, String>,
    },
    ModelsLoaded {
        directory: String,
        result: Result<ModelCatalog, String>,
    },
    SessionCreated {
        request_id: u64,
        result: Result<Session, String>,
    },
    SessionRenamed {
        request_id: u64,
        session_id: String,
        result: Result<Session, String>,
    },
    PromptAccepted {
        request_id: u64,
        session_id: String,
        result: Result<(), String>,
    },
    Aborted {
        session_id: String,
        result: Result<(), String>,
    },
    ActionFinished {
        request_id: String,
        result: Result<(), String>,
    },
    ServerEvent(ServerEnvelope),
}

#[derive(Clone)]
pub struct ApiHandle {
    refresh_commands: Sender<Command>,
    interaction_commands: Sender<Command>,
    abort_commands: Sender<Command>,
    urgent_commands: Sender<Command>,
    _lifetime: Arc<ApiLifetime>,
}

struct ApiLifetime {
    alive: Arc<AtomicBool>,
}

impl Drop for ApiLifetime {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
    }
}

impl ApiHandle {
    pub fn start(config: ApiConfig) -> Result<(Self, Receiver<UiEvent>, String)> {
        let api = Api::new(config)?;
        let server_key = api.base_url.as_str().trim_end_matches('/').to_owned();
        let (refresh_sender, refresh_receiver) = async_channel::unbounded();
        let (interaction_sender, interaction_receiver) = async_channel::unbounded();
        let (abort_sender, abort_receiver) = async_channel::unbounded();
        let (urgent_sender, urgent_receiver) = async_channel::unbounded();
        let (ui_sender, ui_receiver) = async_channel::bounded(UI_EVENT_CAPACITY);
        let alive = Arc::new(AtomicBool::new(true));
        let lifetime = Arc::new(ApiLifetime {
            alive: alive.clone(),
        });

        spawn_command_worker(api.clone(), refresh_receiver, ui_sender.clone());
        spawn_command_worker(api.clone(), interaction_receiver, ui_sender.clone());
        spawn_command_worker(api.clone(), abort_receiver, ui_sender.clone());
        spawn_command_worker(api.clone(), urgent_receiver, ui_sender.clone());
        spawn_event_worker(api, ui_sender, alive);

        Ok((
            Self {
                refresh_commands: refresh_sender,
                interaction_commands: interaction_sender,
                abort_commands: abort_sender,
                urgent_commands: urgent_sender,
                _lifetime: lifetime,
            },
            ui_receiver,
            server_key,
        ))
    }

    pub fn preview() -> (Self, Receiver<UiEvent>, String) {
        let (refresh_sender, refresh_receiver) = async_channel::unbounded();
        let (interaction_sender, interaction_receiver) = async_channel::unbounded();
        let (abort_sender, abort_receiver) = async_channel::unbounded();
        let (urgent_sender, urgent_receiver) = async_channel::unbounded();
        let (ui_sender, ui_receiver) = async_channel::bounded(UI_EVENT_CAPACITY);
        let alive = Arc::new(AtomicBool::new(true));
        let lifetime = Arc::new(ApiLifetime {
            alive: alive.clone(),
        });
        let state = Arc::new(Mutex::new(crate::preview::State::new()));
        spawn_preview_worker(refresh_receiver, ui_sender.clone(), state.clone());
        spawn_preview_worker(interaction_receiver, ui_sender.clone(), state.clone());
        spawn_preview_worker(abort_receiver, ui_sender.clone(), state.clone());
        spawn_preview_worker(urgent_receiver, ui_sender.clone(), state);
        let _ = ui_sender.send_blocking(UiEvent::Connection {
            connected: true,
            error: None,
        });
        (
            Self {
                refresh_commands: refresh_sender,
                interaction_commands: interaction_sender,
                abort_commands: abort_sender,
                urgent_commands: urgent_sender,
                _lifetime: lifetime,
            },
            ui_receiver,
            crate::preview::SERVER_KEY.to_owned(),
        )
    }

    pub fn send(&self, command: Command) {
        let sender = match &command {
            Command::Bootstrap | Command::LoadMessages { .. } | Command::LoadModels { .. } => {
                &self.refresh_commands
            }
            Command::CreateSession { .. }
            | Command::RenameSession { .. }
            | Command::SendPrompt { .. } => &self.interaction_commands,
            Command::Abort { .. } => &self.abort_commands,
            Command::ReplyPermission { .. }
            | Command::ReplyQuestion { .. }
            | Command::RejectQuestion { .. } => &self.urgent_commands,
        };
        let _ = sender.send_blocking(command);
    }
}

#[derive(Clone)]
struct Api {
    base_url: Url,
    client: Client,
    event_client: Client,
    username: String,
    password: Option<String>,
    cloudflare_access: Option<CloudflareAccessHeaders>,
}

#[derive(Clone)]
struct CloudflareAccessHeaders {
    client_id: HeaderValue,
    client_secret: HeaderValue,
}

impl Api {
    fn new(config: ApiConfig) -> Result<Self> {
        let mut base = config.base_url.trim().to_owned();
        if !base.ends_with('/') {
            base.push('/');
        }
        let base_url = Url::parse(&base).context("invalid OpenCode server URL")?;
        if !matches!(base_url.scheme(), "http" | "https") {
            bail!("OpenCode server URL must use http or https");
        }
        if !base_url.username().is_empty() || base_url.password().is_some() {
            bail!("put OpenCode credentials in the username and password options, not the URL");
        }
        if base_url.query().is_some() || base_url.fragment().is_some() {
            bail!("OpenCode server URL must not contain a query string or fragment");
        }
        let loopback = match base_url.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
            None => false,
        };
        if base_url.scheme() == "http" && !loopback {
            bail!("remote servers require HTTPS; use HTTPS or an SSH tunnel to localhost");
        }
        if config.cloudflare_access.is_some() && base_url.scheme() != "https" {
            bail!("Cloudflare Access credentials require an HTTPS server URL");
        }
        let cloudflare_access = config
            .cloudflare_access
            .map(|credentials| {
                let client_id = HeaderValue::from_str(&credentials.client_id)
                    .context("Cloudflare Access client ID contains invalid characters")?;
                let mut client_secret = HeaderValue::from_str(&credentials.client_secret)
                    .context("Cloudflare Access client secret contains invalid characters")?;
                client_secret.set_sensitive(true);
                Ok::<_, anyhow::Error>(CloudflareAccessHeaders {
                    client_id,
                    client_secret,
                })
            })
            .transpose()?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("opencode-gtk/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to initialize HTTP client")?;
        let event_client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(None)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("opencode-gtk/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to initialize event-stream client")?;
        Ok(Self {
            base_url,
            client,
            event_client,
            username: config.username,
            password: config.password,
            cloudflare_access,
        })
    }

    fn request(&self, method: Method, url: Url) -> RequestBuilder {
        let request = self.client.request(method, url);
        self.authenticate(request)
    }

    fn event_request(&self, method: Method, url: Url) -> RequestBuilder {
        let request = self.event_client.request(method, url);
        self.authenticate(request)
    }

    fn authenticate(&self, request: RequestBuilder) -> RequestBuilder {
        let request = match &self.cloudflare_access {
            Some(credentials) => request
                .header("CF-Access-Client-Id", credentials.client_id.clone())
                .header("CF-Access-Client-Secret", credentials.client_secret.clone()),
            None => request,
        };
        match &self.password {
            Some(password) => request.basic_auth(&self.username, Some(password)),
            None => request,
        }
    }

    fn url(&self, path: &str, directory: Option<&str>) -> Result<Url> {
        let mut url = self
            .base_url
            .join(path.trim_start_matches('/'))
            .with_context(|| format!("invalid API path: {path}"))?;
        if let Some(directory) = directory {
            url.query_pairs_mut().append_pair("directory", directory);
        }
        Ok(url)
    }

    fn json(&self, method: Method, url: Url, body: Option<&Value>) -> Result<Value> {
        let request = self.request(method, url);
        let response = match body {
            Some(body) => request.json(body).send(),
            None => request.send(),
        }
        .context("request failed")?;
        parse_json(response)
    }

    fn empty(&self, method: Method, url: Url, body: Option<&Value>) -> Result<()> {
        let request = self.request(method, url);
        let response = match body {
            Some(body) => request.json(body).send(),
            None => request.send(),
        }
        .context("request failed")?;
        expect_success(response).map(|_| ())
    }

    fn complete_request(&self, method: Method, url: Url, body: Option<&Value>) -> Result<()> {
        let request = self.request(method, url);
        let response = match body {
            Some(body) => request.json(body).send(),
            None => request.send(),
        }
        .context("request failed")?;
        if matches!(response.status(), StatusCode::NOT_FOUND | StatusCode::GONE) {
            return Ok(());
        }
        expect_success(response).map(|_| ())
    }

    fn bootstrap(&self) -> Result<Bootstrap> {
        let mut warnings = Vec::new();
        let mut retry_needed = false;
        let health = self.json(Method::GET, self.url("global/health", None)?, None)?;
        let version = health
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();

        let projects_result = self
            .json(Method::GET, self.url("project", None)?, None)
            .and_then(|value| {
                serde_json::from_value(value).context("server returned an invalid project list")
            });
        let mut sessions_complete = projects_result.is_ok();
        let projects: Vec<Project> = match projects_result {
            Ok(projects) => projects,
            Err(error) => {
                retry_needed = true;
                warnings.push(format!("Could not list every project: {error}"));
                Vec::new()
            }
        };
        let project_directories: HashSet<_> = projects
            .iter()
            .map(|project| project.worktree.clone())
            .collect();
        let mut sessions_by_id = HashMap::new();
        let mut first_session_error = None;

        if project_directories.is_empty() {
            match self.list_sessions(None, false) {
                Ok((sessions, complete)) => {
                    sessions_complete &= complete;
                    if !complete {
                        warnings.push("The server session list reached its result limit".into());
                    }
                    for session in sessions {
                        sessions_by_id.insert(session.id.clone(), session);
                    }
                }
                Err(error) => first_session_error = Some(error),
            }
        } else {
            for directory in &project_directories {
                match self.list_sessions(Some(directory), true) {
                    Ok((sessions, complete)) => {
                        sessions_complete &= complete;
                        if !complete {
                            warnings.push(format!(
                                "The session list for {directory} reached its result limit"
                            ));
                        }
                        for session in sessions {
                            let replace =
                                sessions_by_id
                                    .get(&session.id)
                                    .is_none_or(|existing: &Session| {
                                        session.time.updated >= existing.time.updated
                                    });
                            if replace {
                                sessions_by_id.insert(session.id.clone(), session);
                            }
                        }
                    }
                    Err(error) => {
                        retry_needed = true;
                        sessions_complete = false;
                        warnings.push(format!(
                            "Could not refresh sessions for {directory}: {error}"
                        ));
                        if first_session_error.is_none() {
                            first_session_error = Some(error);
                        }
                    }
                }
            }
        }
        if sessions_by_id.is_empty() {
            if let Some(error) = first_session_error {
                return Err(error);
            }
        }
        let mut sessions: Vec<_> = sessions_by_id.into_values().collect();
        sessions.retain(|session| session.time.archived.is_none() && session.parent_id.is_none());
        sessions.sort_by_key(|session| std::cmp::Reverse(session.time.updated));

        let mut directories = project_directories;
        directories.extend(sessions.iter().map(|session| session.directory.clone()));
        let mut statuses = HashMap::new();
        let mut statuses_complete = true;
        let mut pending_complete = true;
        let mut pending = Vec::new();
        for directory in &directories {
            match self.load_statuses(directory) {
                Ok(directory_statuses) => statuses.extend(directory_statuses),
                Err(error) => {
                    retry_needed = true;
                    statuses_complete = false;
                    warnings.push(format!(
                        "Could not refresh session status for {directory}: {error}"
                    ));
                }
            }
            match self.load_pending("permission", "permission.asked", directory) {
                Ok(requests) => pending.extend(requests),
                Err(error) => {
                    retry_needed = true;
                    pending_complete = false;
                    warnings.push(format!(
                        "Could not recover permission requests for {directory}: {error}"
                    ));
                }
            }
            match self.load_pending("question", "question.asked", directory) {
                Ok(requests) => pending.extend(requests),
                Err(error) => {
                    retry_needed = true;
                    pending_complete = false;
                    warnings.push(format!(
                        "Could not recover questions for {directory}: {error}"
                    ));
                }
            }
        }

        Ok(Bootstrap {
            version,
            sessions,
            sessions_complete,
            projects,
            statuses,
            statuses_complete,
            pending,
            pending_complete,
            retry_needed,
            warnings,
        })
    }

    fn list_sessions(
        &self,
        directory: Option<&str>,
        project_scope: bool,
    ) -> Result<(Vec<Session>, bool)> {
        let mut url = self.url("session", directory)?;
        url.query_pairs_mut()
            .append_pair("roots", "true")
            .append_pair("limit", &SESSION_LIST_LIMIT.to_string());
        if project_scope {
            url.query_pairs_mut().append_pair("scope", "project");
        }
        let value = self.json(Method::GET, url, None)?;
        let sessions: Vec<Session> =
            serde_json::from_value(value).context("server returned an invalid session list")?;
        let complete = sessions.len() < SESSION_LIST_LIMIT;
        Ok((sessions, complete))
    }

    fn load_statuses(&self, directory: &str) -> Result<HashMap<String, RunStatus>> {
        let value = self.json(
            Method::GET,
            self.url("session/status", Some(directory))?,
            None,
        )?;
        let values = value
            .as_object()
            .context("server returned an invalid session status map")?;
        Ok(values
            .iter()
            .filter_map(|(session_id, value)| {
                RunStatus::from_value(value).map(|status| (session_id.clone(), status))
            })
            .collect())
    }

    fn load_pending(
        &self,
        path: &str,
        event_type: &str,
        directory: &str,
    ) -> Result<Vec<ServerEnvelope>> {
        let value = self.json(Method::GET, self.url(path, Some(directory))?, None)?;
        let requests = value
            .as_array()
            .context("server returned an invalid pending-request list")?;
        Ok(requests
            .iter()
            .cloned()
            .map(|properties| ServerEnvelope {
                directory: Some(directory.to_owned()),
                payload: json!({
                    "type": event_type,
                    "properties": properties,
                }),
            })
            .collect())
    }

    fn load_messages(
        &self,
        session_id: &str,
        directory: &str,
        before: Option<&str>,
    ) -> Result<MessagePage> {
        let mut url = self.url(&format!("session/{session_id}/message"), Some(directory))?;
        url.query_pairs_mut()
            .append_pair("limit", &MESSAGE_PAGE_SIZE.to_string());
        if let Some(before) = before {
            url.query_pairs_mut().append_pair("before", before);
        }
        let response = expect_success(
            self.request(Method::GET, url)
                .send()
                .context("failed to load messages")?,
        )?;
        let next_cursor = response
            .headers()
            .get("X-Next-Cursor")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let messages = response
            .json::<Vec<Value>>()
            .context("server returned invalid messages")?;
        Ok(MessagePage {
            messages,
            next_cursor,
        })
    }

    fn load_models(&self, directory: &str) -> Result<ModelCatalog> {
        let providers = self.json(
            Method::GET,
            self.url("config/providers", Some(directory))?,
            None,
        )?;
        let config = self
            .json(Method::GET, self.url("config", Some(directory))?, None)
            .unwrap_or(Value::Null);
        Ok(ModelCatalog::from_values(&providers, &config))
    }

    fn create_session(&self, directory: &str, title: Option<&str>) -> Result<Session> {
        let body = match title.filter(|title| !title.trim().is_empty()) {
            Some(title) => json!({ "title": title.trim() }),
            None => json!({}),
        };
        let value = self.json(
            Method::POST,
            self.url("session", Some(directory))?,
            Some(&body),
        )?;
        serde_json::from_value(value).context("server returned an invalid session")
    }

    fn rename_session(&self, session_id: &str, directory: &str, title: &str) -> Result<Session> {
        let value = self.json(
            Method::PATCH,
            self.url(&format!("session/{session_id}"), Some(directory))?,
            Some(&json!({ "title": title.trim() })),
        )?;
        serde_json::from_value(value).context("server returned an invalid session")
    }

    fn send_prompt(
        &self,
        session_id: &str,
        directory: &str,
        text: &str,
        selection: &ModelSelection,
        agent: Option<&str>,
        attachments: &[PathBuf],
    ) -> Result<()> {
        let mut parts = vec![json!({ "type": "text", "text": text })];
        parts.extend(encode_attachments(attachments)?);
        let mut body = json!({
            "model": {
                "providerID": selection.provider_id,
                "modelID": selection.model_id
            },
            "parts": parts
        });
        if let Some(variant) = &selection.variant {
            body["variant"] = Value::String(variant.clone());
        }
        if let Some(agent) = agent {
            body["agent"] = Value::String(agent.to_owned());
        }
        self.empty(
            Method::POST,
            self.url(
                &format!("session/{session_id}/prompt_async"),
                Some(directory),
            )?,
            Some(&body),
        )
    }

    fn abort(&self, session_id: &str, directory: &str) -> Result<()> {
        self.empty(
            Method::POST,
            self.url(&format!("session/{session_id}/abort"), Some(directory))?,
            None,
        )
    }

    fn reply_permission(&self, request_id: &str, directory: &str, reply: &str) -> Result<()> {
        self.complete_request(
            Method::POST,
            self.url(&format!("permission/{request_id}/reply"), Some(directory))?,
            Some(&json!({ "reply": reply })),
        )
    }

    fn reply_question(
        &self,
        request_id: &str,
        directory: &str,
        answers: &[Vec<String>],
    ) -> Result<()> {
        self.complete_request(
            Method::POST,
            self.url(&format!("question/{request_id}/reply"), Some(directory))?,
            Some(&json!({ "answers": answers })),
        )
    }

    fn reject_question(&self, request_id: &str, directory: &str) -> Result<()> {
        self.complete_request(
            Method::POST,
            self.url(&format!("question/{request_id}/reject"), Some(directory))?,
            None,
        )
    }
}

fn spawn_preview_worker(
    commands: Receiver<Command>,
    ui: Sender<UiEvent>,
    state: Arc<Mutex<crate::preview::State>>,
) {
    thread::spawn(move || {
        while let Ok(command) = commands.recv_blocking() {
            let event = state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .handle(command);
            if ui.send_blocking(event).is_err() {
                break;
            }
        }
    });
}

fn spawn_command_worker(api: Api, commands: Receiver<Command>, ui: Sender<UiEvent>) {
    thread::spawn(move || {
        while let Ok(command) = commands.recv_blocking() {
            let event = match command {
                Command::Bootstrap => UiEvent::Bootstrap(api.bootstrap().map_err(format_error)),
                Command::LoadMessages {
                    session_id,
                    directory,
                    before,
                } => {
                    let result = api
                        .load_messages(&session_id, &directory, before.as_deref())
                        .map_err(format_error);
                    UiEvent::MessagesLoaded {
                        session_id,
                        before,
                        result,
                    }
                }
                Command::LoadModels { directory } => {
                    let result = api.load_models(&directory).map_err(format_error);
                    UiEvent::ModelsLoaded { directory, result }
                }
                Command::CreateSession {
                    request_id,
                    directory,
                    title,
                } => UiEvent::SessionCreated {
                    request_id,
                    result: api
                        .create_session(&directory, title.as_deref())
                        .map_err(format_error),
                },
                Command::RenameSession {
                    request_id,
                    session_id,
                    directory,
                    title,
                } => UiEvent::SessionRenamed {
                    request_id,
                    result: api
                        .rename_session(&session_id, &directory, &title)
                        .map_err(format_error),
                    session_id,
                },
                Command::SendPrompt {
                    request_id,
                    session_id,
                    directory,
                    text,
                    selection,
                    agent,
                    attachments,
                } => {
                    let result = api
                        .send_prompt(
                            &session_id,
                            &directory,
                            &text,
                            &selection,
                            agent.as_deref(),
                            &attachments,
                        )
                        .map_err(format_error);
                    UiEvent::PromptAccepted {
                        request_id,
                        session_id,
                        result,
                    }
                }
                Command::Abort {
                    session_id,
                    directory,
                } => {
                    let result = api.abort(&session_id, &directory).map_err(format_error);
                    UiEvent::Aborted { session_id, result }
                }
                Command::ReplyPermission {
                    request_id,
                    directory,
                    reply,
                } => {
                    let result = api
                        .reply_permission(&request_id, &directory, &reply)
                        .map_err(format_error);
                    UiEvent::ActionFinished { request_id, result }
                }
                Command::ReplyQuestion {
                    request_id,
                    directory,
                    answers,
                } => {
                    let result = api
                        .reply_question(&request_id, &directory, &answers)
                        .map_err(format_error);
                    UiEvent::ActionFinished { request_id, result }
                }
                Command::RejectQuestion {
                    request_id,
                    directory,
                } => {
                    let result = api
                        .reject_question(&request_id, &directory)
                        .map_err(format_error);
                    UiEvent::ActionFinished { request_id, result }
                }
            };
            if ui.send_blocking(event).is_err() {
                break;
            }
        }
    });
}

fn spawn_event_worker(api: Api, ui: Sender<UiEvent>, alive: Arc<AtomicBool>) {
    thread::spawn(move || {
        let mut delay = Duration::from_millis(500);
        let mut last_error = None;
        while alive.load(Ordering::Relaxed) {
            let mut connected = false;
            let attempt_started = Instant::now();
            let result = stream_events(&api, &ui, &mut connected);
            if !alive.load(Ordering::Relaxed) {
                return;
            }
            if connected {
                last_error = None;
                if attempt_started.elapsed() >= Duration::from_secs(30) {
                    delay = Duration::from_millis(500);
                }
            }
            let error = result.err().map(format_error);
            if error != last_error {
                let _ = ui.send_blocking(UiEvent::Connection {
                    connected: false,
                    error: error.clone(),
                });
                last_error = error;
            }
            thread::sleep(delay);
            delay = (delay * 2).min(Duration::from_secs(10));
        }
    });
}

fn stream_events(api: &Api, ui: &Sender<UiEvent>, connected: &mut bool) -> Result<()> {
    let response = expect_success(
        api.event_request(Method::GET, api.url("global/event", None)?)
            .header("Accept", "text/event-stream")
            .send()
            .context("failed to connect event stream")?,
    )?;
    let mut reader = BufReader::new(response);
    let mut decoder = SseDecoder::default();
    let mut line = String::new();

    loop {
        if ui.is_closed() {
            return Ok(());
        }
        line.clear();
        if reader
            .read_line(&mut line)
            .context("failed to read event stream")?
            == 0
        {
            bail!("event stream closed");
        }
        if ui.is_closed() {
            return Ok(());
        }
        let Some(data) = decoder.push(&line) else {
            continue;
        };
        let envelope: Value = serde_json::from_str(&data).context("invalid event payload")?;
        let directory = envelope
            .get("directory")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let payload = envelope.get("payload").cloned().unwrap_or(envelope);
        match payload.get("type").and_then(Value::as_str) {
            Some("server.connected") => {
                *connected = true;
                let _ = ui.send_blocking(UiEvent::Connection {
                    connected: true,
                    error: None,
                });
            }
            Some("server.heartbeat") | Some("sync") => {}
            _ => {
                ui.send_blocking(UiEvent::ServerEvent(ServerEnvelope { directory, payload }))
                    .map_err(|_| anyhow!("UI event receiver closed"))?;
            }
        }
    }
}

#[derive(Default)]
struct SseDecoder {
    data: Vec<String>,
}

impl SseDecoder {
    fn push(&mut self, line: &str) -> Option<String> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if self.data.is_empty() {
                return None;
            }
            return Some(std::mem::take(&mut self.data).join("\n"));
        }
        if let Some(value) = line.strip_prefix("data:") {
            self.data
                .push(value.strip_prefix(' ').unwrap_or(value).to_owned());
        }
        None
    }
}

fn encode_attachments(paths: &[PathBuf]) -> Result<Vec<Value>> {
    let mut total = 0_u64;
    paths
        .iter()
        .map(|path| {
            let file = File::open(path)
                .with_context(|| format!("failed to open attachment {}", path.display()))?;
            if !file
                .metadata()
                .with_context(|| format!("failed to inspect attachment {}", path.display()))?
                .is_file()
            {
                bail!("attachment {} is not a regular file", path.display());
            }
            let mut bytes = Vec::new();
            file.take(MAX_ATTACHMENT_BYTES + 1)
                .read_to_end(&mut bytes)
                .with_context(|| format!("failed to read attachment {}", path.display()))?;
            if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
                bail!(
                    "attachment {} is larger than 25 MiB",
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("file")
                );
            }
            total += bytes.len() as u64;
            if total > MAX_TOTAL_ATTACHMENT_BYTES {
                bail!("attachments are larger than 40 MiB in total");
            }
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("attachment");
            Ok(json!({
                "type": "file",
                "mime": mime.as_ref(),
                "filename": filename,
                "url": format!("data:{};base64,{}", mime.as_ref(), BASE64.encode(bytes))
            }))
        })
        .collect()
}

fn expect_success(response: Response) -> Result<Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/data/message")
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.trim().to_owned());
    if detail.is_empty() {
        bail!("server returned {status}");
    }
    bail!("server returned {status}: {detail}")
}

fn parse_json(response: Response) -> Result<Value> {
    expect_success(response)?
        .json()
        .context("server returned invalid JSON")
}

fn format_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader, Read, Write},
        net::{TcpListener, TcpStream},
        sync::{Arc, Mutex},
    };

    use super::*;

    #[test]
    fn preview_handle_answers_without_a_server() {
        let (api, events, key) = ApiHandle::preview();
        assert_eq!(key, crate::preview::SERVER_KEY);
        api.send(Command::Bootstrap);
        api.send(Command::LoadModels {
            directory: "/repo".into(),
        });
        let mut saw_bootstrap = false;
        let mut saw_models = false;
        for _ in 0..16 {
            match events.recv_blocking() {
                Ok(UiEvent::Connection {
                    connected: true, ..
                }) => {}
                Ok(UiEvent::Bootstrap(Ok(bootstrap))) => {
                    assert_eq!(bootstrap.version, "preview");
                    assert!(!bootstrap.sessions.is_empty());
                    saw_bootstrap = true;
                }
                Ok(UiEvent::ModelsLoaded {
                    result: Ok(catalog),
                    ..
                }) => {
                    assert!(catalog
                        .models
                        .iter()
                        .any(|model| model.supports_attachments));
                    saw_models = true;
                }
                Ok(_) => {}
                Err(_) => panic!("preview event channel closed"),
            }
            if saw_bootstrap && saw_models {
                return;
            }
        }
        panic!("missing preview bootstrap or models");
    }

    fn config(base_url: String, password: Option<&str>) -> ApiConfig {
        ApiConfig {
            base_url,
            username: "opencode".into(),
            password: password.map(str::to_owned),
            cloudflare_access: None,
        }
    }

    fn read_http_request(stream: &TcpStream) -> String {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut content_length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some(value) = line
                .strip_prefix("Content-Length:")
                .or_else(|| line.strip_prefix("content-length:"))
            {
                content_length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).unwrap();
        request_line
    }

    #[test]
    fn decoder_joins_multiline_data_and_ignores_comments() {
        let mut decoder = SseDecoder::default();
        assert_eq!(decoder.push(": keepalive\n"), None);
        assert_eq!(decoder.push("data: {\"hello\":\n"), None);
        assert_eq!(decoder.push("data: \"world\"}\n"), None);
        assert_eq!(decoder.push("\n"), Some("{\"hello\":\n\"world\"}".into()));
    }

    #[test]
    fn attachment_becomes_a_data_url() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.txt");
        fs::write(&path, "hello").unwrap();

        let parts = encode_attachments(&[path]).unwrap();

        assert_eq!(parts[0]["type"], "file");
        assert_eq!(parts[0]["filename"], "note.txt");
        assert!(parts[0]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:text/plain;base64,"));
    }

    #[test]
    fn rejects_credentials_in_urls_and_remote_cleartext_http() {
        assert!(Api::new(config("http://alice:secret@127.0.0.1:4096".into(), None)).is_err());
        assert!(Api::new(config("http://example.com:4096".into(), Some("secret"))).is_err());
        assert!(Api::new(config("http://example.com:4096".into(), None)).is_err());
        assert!(Api::new(config("http://127.0.0.1:4096".into(), Some("secret"))).is_ok());
        assert!(Api::new(config("https://example.com:4096".into(), Some("secret"))).is_ok());

        let mut cleartext_cloudflare = config("http://127.0.0.1:4096".into(), None);
        cleartext_cloudflare.cloudflare_access = Some(
            CloudflareAccessCredentials::new("client.access".into(), "secret".into()).unwrap(),
        );
        assert!(Api::new(cleartext_cloudflare).is_err());
    }

    #[test]
    fn cloudflare_access_headers_cover_api_and_event_requests() {
        let mut config = config("https://opencode.example.com".into(), None);
        config.cloudflare_access = Some(
            CloudflareAccessCredentials::new("client.access".into(), "secret".into()).unwrap(),
        );
        let api = Api::new(config).unwrap();

        for request in [
            api.request(Method::GET, api.url("global/health", None).unwrap()),
            api.event_request(Method::GET, api.url("global/event", None).unwrap()),
        ] {
            let request = request.build().unwrap();
            assert_eq!(request.headers()["CF-Access-Client-Id"], "client.access");
            assert_eq!(request.headers()["CF-Access-Client-Secret"], "secret");
        }
    }

    #[test]
    fn completing_an_already_resolved_request_succeeds() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_http_request(&stream);
            write!(
                stream,
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });

        let api = Api::new(config(format!("http://{address}"), None)).unwrap();
        let result = api.reply_permission("per_resolved", "/repo", "reject");
        assert!(result.is_ok(), "{result:?}");
        server.join().unwrap();
    }

    #[test]
    fn does_not_follow_api_redirects() {
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let target_address = target.local_addr().unwrap();
        target.set_nonblocking(true).unwrap();
        let target_seen = Arc::new(AtomicBool::new(false));
        let seen = target_seen.clone();
        let target_server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < deadline {
                match target.accept() {
                    Ok((mut stream, _)) => {
                        seen.store(true, Ordering::Relaxed);
                        read_http_request(&stream);
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                        .unwrap();
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("target server failed: {error}"),
                }
            }
        });

        let redirect = TcpListener::bind("127.0.0.1:0").unwrap();
        let redirect_address = redirect.local_addr().unwrap();
        let redirect_server = std::thread::spawn(move || {
            let (mut stream, _) = redirect.accept().unwrap();
            read_http_request(&stream);
            write!(
                stream,
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target_address}/leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });

        let api = Api::new(config(format!("http://{redirect_address}"), Some("secret"))).unwrap();
        assert!(api
            .reply_permission("per_redirect", "/repo", "always")
            .is_err());
        redirect_server.join().unwrap();
        target_server.join().unwrap();
        assert!(!target_seen.load(Ordering::Relaxed));
    }

    #[test]
    fn bootstrap_merges_projects_and_recovers_pending_requests() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..10 {
                let (mut stream, _) = listener.accept().unwrap();
                let request_line = read_http_request(&stream);
                captured.lock().unwrap().push(request_line.clone());
                let directory = if request_line.contains("directory=%2Fa") {
                    "a"
                } else if request_line.contains("directory=%2Fb") {
                    "b"
                } else {
                    ""
                };
                let body = if request_line.contains(" /global/health ") {
                    json!({ "version": "1.18.15" })
                } else if request_line.contains(" /project ") {
                    json!([
                        { "worktree": "/a", "name": "A" },
                        { "worktree": "/b", "name": "B" }
                    ])
                } else if request_line.contains(" /session/status?") {
                    json!({ format!("ses_{directory}"): { "type": "busy" } })
                } else if request_line.contains(" /permission?") {
                    json!([{ "id": format!("per_{directory}"), "sessionID": format!("ses_{directory}") }])
                } else if request_line.contains(" /question?") {
                    json!([{ "id": format!("que_{directory}"), "sessionID": format!("ses_{directory}"), "questions": [] }])
                } else if request_line.contains(" /session?") {
                    json!([{
                        "id": format!("ses_{directory}"),
                        "directory": format!("/{directory}"),
                        "title": directory.to_uppercase(),
                        "time": { "created": 1, "updated": 2 }
                    }])
                } else {
                    panic!("unexpected request: {request_line}");
                };
                let body = body.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });

        let api = Api::new(config(format!("http://{address}"), None)).unwrap();
        let bootstrap = api.bootstrap().unwrap();
        server.join().unwrap();

        let mut session_ids: Vec<_> = bootstrap
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect();
        session_ids.sort_unstable();
        assert_eq!(session_ids, ["ses_a", "ses_b"]);
        assert!(bootstrap.sessions_complete);
        assert!(bootstrap.statuses_complete);
        assert!(bootstrap.pending_complete);
        assert!(!bootstrap.retry_needed);
        assert_eq!(bootstrap.statuses.len(), 2);
        assert_eq!(bootstrap.pending.len(), 4);
        assert!(requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.contains(" /session?"))
            .all(|request| request.contains("scope=project") && request.contains("roots=true")));
    }
}
