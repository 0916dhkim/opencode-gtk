use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_channel::Receiver;
use cosmic::app::{ContextDrawer, Core, Task, context_drawer};
use cosmic::iced::{Alignment, Length, Subscription};
use cosmic::widget::{button, column, container, row, scrollable, text, text_input};
use cosmic::{Application, Element};
use serde::Deserialize;

use crate::{
    Args,
    api::{ApiConfig, ApiHandle, Command, UiEvent},
    credentials::CloudflareAccessCredentials,
    jobs::{self, JobKind},
    markdown,
    model::{self, Conversation, ModelCatalog, Role, RunStatus, Session, TrayItem},
    persist::{PersistedState, default_path},
    preview, protocol,
    tray::{RowAction, SendMode},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawerPage {
    Jobs,
    Sessions,
    Settings,
}

pub struct OpenCodeCosmic {
    core: Core,
    args: Args,
    api: Option<ApiHandle>,
    receiver: Option<Receiver<UiEvent>>,
    mock_server: Option<preview::State>,
    state: PersistedState,
    sessions: HashMap<String, Session>,
    tabs: Vec<String>,
    active_session_id: Option<String>,
    conversations: HashMap<String, Conversation>,
    catalogs: HashMap<String, ModelCatalog>,
    statuses: HashMap<String, RunStatus>,
    jobs: jobs::Jobs,
    composer_text: String,
    search_query: String,
    active_drawer: Option<DrawerPage>,
    connection_status: String,
    error_banner: Option<String>,
    server_url_input: String,
    username_input: String,
    password_input: String,
    next_req_id: u64,
}

#[derive(Clone, Debug)]
pub enum Message {
    Tick,
    SelectTab(String),
    CloseTab(String),
    NewSession,
    ComposerInput(String),
    SendPrompt(SendMode),
    StopSession,
    TrayAction(String, RowAction),
    TrayClear,
    CopyText(String),
    ToggleDrawer(DrawerPage),
    CloseDrawer,
    SearchInput(String),
    SelectSession(String),
    SelectModel(String),
    SettingsUrlInput(String),
    SettingsUsernameInput(String),
    SettingsPasswordInput(String),
    ApplySettings,
    DismissError,
}

impl Application for OpenCodeCosmic {
    type Executor = cosmic::iced::executor::Default;
    type Flags = Args;
    type Message = Message;
    const APP_ID: &'static str = "ai.opencode.Cosmic";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let (state, _) = PersistedState::load(&default_path()).unwrap_or_default();
        let server_url = flags
            .server
            .clone()
            .unwrap_or_else(|| state.connection.server.clone());
        let username = flags
            .username
            .clone()
            .unwrap_or_else(|| state.connection.username.clone());

        let mut app = Self {
            core,
            args: flags.clone(),
            api: None,
            receiver: None,
            mock_server: None,
            state,
            sessions: HashMap::new(),
            tabs: Vec::new(),
            active_session_id: None,
            conversations: HashMap::new(),
            catalogs: HashMap::new(),
            statuses: HashMap::new(),
            jobs: jobs::Jobs::default(),
            composer_text: String::new(),
            search_query: String::new(),
            active_drawer: None,
            connection_status: "Connecting...".to_string(),
            error_banner: None,
            server_url_input: server_url,
            username_input: username,
            password_input: flags.password.clone().unwrap_or_default(),
            next_req_id: 1,
        };

        if flags.preview {
            let mut mock = preview::State::new();
            let initial_events = mock.take_server_events();
            app.connection_status = "Preview (Offline)".to_string();

            let s_state = preview::server_state();
            for tab in &s_state.tabs {
                app.tabs.push(tab.id.clone());
            }
            app.active_session_id = s_state.active;

            for event in initial_events {
                app.handle_ui_event(event);
            }
            app.mock_server = Some(mock);
        } else {
            app.connect_api();
        }

        (app, Task::none())
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Tick => {
                self.drain_events();
                Task::none()
            }
            Message::SelectTab(id) => {
                self.set_active_session(&id);
                Task::none()
            }
            Message::CloseTab(id) => {
                self.close_tab(&id);
                Task::none()
            }
            Message::NewSession => {
                self.create_session();
                Task::none()
            }
            Message::ComposerInput(val) => {
                self.composer_text = val;
                Task::none()
            }
            Message::SendPrompt(mode) => {
                self.send_composer_prompt(mode);
                Task::none()
            }
            Message::StopSession => {
                self.stop_active_session();
                Task::none()
            }
            Message::TrayAction(id, action) => {
                self.handle_tray_action(&id, action);
                Task::none()
            }
            Message::TrayClear => {
                self.clear_tray();
                Task::none()
            }
            Message::CopyText(txt) => cosmic::iced::clipboard::write(txt),
            Message::ToggleDrawer(page) => {
                if self.active_drawer == Some(page) {
                    self.active_drawer = None;
                } else {
                    self.active_drawer = Some(page);
                }
                Task::none()
            }
            Message::CloseDrawer => {
                self.active_drawer = None;
                Task::none()
            }
            Message::SearchInput(q) => {
                self.search_query = q;
                Task::none()
            }
            Message::SelectSession(id) => {
                self.open_session(&id);
                self.active_drawer = None;
                Task::none()
            }
            Message::SelectModel(model_id) => {
                self.switch_model(&model_id);
                Task::none()
            }
            Message::SettingsUrlInput(url) => {
                self.server_url_input = url;
                Task::none()
            }
            Message::SettingsUsernameInput(user) => {
                self.username_input = user;
                Task::none()
            }
            Message::SettingsPasswordInput(pwd) => {
                self.password_input = pwd;
                Task::none()
            }
            Message::ApplySettings => {
                self.reconnect_with_settings();
                Task::none()
            }
            Message::DismissError => {
                self.error_banner = None;
                Task::none()
            }
        }
    }

    fn header_start(&self) -> Vec<Element<'_, Self::Message>> {
        let mut tab_buttons = Vec::new();

        for tab_id in &self.tabs {
            let title = self
                .sessions
                .get(tab_id)
                .map(|s| s.title.as_str())
                .unwrap_or(tab_id.as_str());

            let is_busy = self.is_session_busy(tab_id);

            let label = if is_busy {
                format!("⏳ {title}")
            } else {
                title.to_string()
            };

            let id_clone = tab_id.clone();
            let close_id = tab_id.clone();

            let btn = button::text(label)
                .on_press(Message::SelectTab(id_clone))
                .padding([4, 8]);

            let close_btn = button::text("✕")
                .on_press(Message::CloseTab(close_id))
                .padding([2, 4]);

            let tab_box = row::with_children(vec![btn.into(), close_btn.into()])
                .align_y(Alignment::Center)
                .spacing(2);

            let tab_container = container(tab_box).padding(2);
            tab_buttons.push(tab_container.into());
        }

        tab_buttons.push(
            button::text("+")
                .on_press(Message::NewSession)
                .padding([4, 8])
                .into(),
        );

        tab_buttons
    }

    fn header_center(&self) -> Vec<Element<'_, Self::Message>> {
        let active_model = self.active_session_model_label();
        vec![
            button::text(format!("Model: {active_model}"))
                .on_press(Message::ToggleDrawer(DrawerPage::Settings))
                .padding([4, 10])
                .into(),
        ]
    }

    fn header_end(&self) -> Vec<Element<'_, Self::Message>> {
        let usage = self.active_context_usage();
        let running_jobs = self.active_jobs_count();

        let jobs_label = if running_jobs > 0 {
            format!("⚡ Jobs ({running_jobs})")
        } else {
            "Jobs".to_string()
        };

        vec![
            text(usage).size(12).into(),
            button::text(jobs_label)
                .on_press(Message::ToggleDrawer(DrawerPage::Jobs))
                .padding([4, 8])
                .into(),
            button::text("Sessions")
                .on_press(Message::ToggleDrawer(DrawerPage::Sessions))
                .padding([4, 8])
                .into(),
            button::text("⚙ Settings")
                .on_press(Message::ToggleDrawer(DrawerPage::Settings))
                .padding([4, 8])
                .into(),
        ]
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let mut main_items = Vec::new();

        if let Some(err) = &self.error_banner {
            let banner = row::with_children(vec![
                text(format!("⚠ {err}")).size(13).width(Length::Fill).into(),
                button::text("Dismiss")
                    .on_press(Message::DismissError)
                    .into(),
            ])
            .padding(8)
            .spacing(8);

            main_items.push(container(banner).padding(4).into());
        }

        if let Some(active_id) = &self.active_session_id {
            let conversation = self.conversations.get(active_id);
            let tray_items: Vec<TrayItem> =
                conversation.map(|c| c.tray_items()).unwrap_or_default();
            let is_busy = self.is_session_busy(active_id);

            let mut message_elements = Vec::new();

            if let Some(conv) = conversation {
                for message in &conv.messages {
                    if message.role == Role::User && tray_items.iter().any(|t| t.id == message.id) {
                        continue;
                    }

                    let is_user = message.role == Role::User;
                    let role_badge = if is_user { "You" } else { "OpenCode" };

                    let mut turn_items = Vec::new();
                    turn_items.push(text(role_badge).size(12).into());

                    let rendered_content = message.render();
                    if !rendered_content.is_empty() {
                        turn_items.push(markdown::render_markdown(
                            &rendered_content,
                            Message::CopyText,
                        ));
                    }

                    let turn_col = column::with_children(turn_items).spacing(6);
                    let card = container(turn_col).padding(12).width(Length::Fill);

                    message_elements.push(card.into());
                }
            }

            if is_busy {
                let busy_indicator = row::with_children(vec![
                    text("⏳ OpenCode is thinking...").size(13).into(),
                    button::text("Stop").on_press(Message::StopSession).into(),
                ])
                .spacing(8)
                .padding(8);

                message_elements.push(container(busy_indicator).padding(4).into());
            }

            let message_list = column::with_children(message_elements)
                .spacing(12)
                .padding(16);

            let transcript_scroll = scrollable(message_list)
                .width(Length::Fill)
                .height(Length::Fill);

            main_items.push(transcript_scroll.into());

            // Steer/Queue Tray
            if !tray_items.is_empty() {
                let mut tray_rows = Vec::new();
                tray_rows.push(
                    row::with_children(vec![
                        text(format!("Waiting ({}):", tray_items.len()))
                            .size(12)
                            .width(Length::Fill)
                            .into(),
                        button::text("Clear all")
                            .on_press(Message::TrayClear)
                            .into(),
                    ])
                    .align_y(Alignment::Center)
                    .into(),
                );

                for item in &tray_items {
                    let delivery_label = match item.delivery {
                        protocol::Delivery::Steer => "Steer",
                        protocol::Delivery::Queue => "Queue",
                        _ => "Steer",
                    };

                    let preview_text = if item.text.len() > 60 {
                        format!("{}...", &item.text[..60])
                    } else {
                        item.text.clone()
                    };

                    let item_row = row::with_children(vec![
                        text(delivery_label).size(12).into(),
                        text(preview_text).size(13).width(Length::Fill).into(),
                        button::text("Switch")
                            .on_press(Message::TrayAction(item.id.clone(), RowAction::Switch))
                            .into(),
                        button::text("✕")
                            .on_press(Message::TrayAction(item.id.clone(), RowAction::Cancel))
                            .into(),
                    ])
                    .spacing(8)
                    .align_y(Alignment::Center);

                    tray_rows.push(container(item_row).padding(4).into());
                }

                let tray_col = column::with_children(tray_rows).spacing(4).padding(8);
                main_items.push(container(tray_col).padding(4).into());
            }

            // Composer area
            let send_buttons = if is_busy {
                row::with_children(vec![
                    button::text("Stop").on_press(Message::StopSession).into(),
                    button::text("Steer (Enter)")
                        .on_press(Message::SendPrompt(SendMode::Steer))
                        .into(),
                    button::text("Queue (Ctrl+Enter)")
                        .on_press(Message::SendPrompt(SendMode::Queue))
                        .into(),
                ])
                .spacing(6)
            } else {
                row::with_children(vec![
                    button::text("Send")
                        .on_press(Message::SendPrompt(SendMode::Send))
                        .into(),
                ])
                .spacing(6)
            };

            let composer_row = row::with_children(vec![
                text_input("Ask OpenCode...", &self.composer_text)
                    .on_input(Message::ComposerInput)
                    .on_submit(|_| Message::SendPrompt(SendMode::Send))
                    .width(Length::Fill)
                    .into(),
                send_buttons.into(),
            ])
            .spacing(8)
            .padding(12)
            .align_y(Alignment::Center);

            main_items.push(container(composer_row).padding(4).into());
        } else {
            let empty_view = column::with_children(vec![
                text("Welcome to OpenCode COSMIC").size(20).into(),
                text(format!("Status: {}", self.connection_status))
                    .size(14)
                    .into(),
                button::text("Create New Session")
                    .on_press(Message::NewSession)
                    .padding([8, 16])
                    .into(),
            ])
            .spacing(16)
            .padding(32)
            .align_x(Alignment::Center);

            main_items.push(
                container(empty_view)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center)
                    .into(),
            );
        }

        let main_content = column::with_children(main_items)
            .width(Length::Fill)
            .height(Length::Fill);

        container(main_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn context_drawer(&self) -> Option<ContextDrawer<'_, Self::Message>> {
        let page = self.active_drawer?;
        match page {
            DrawerPage::Jobs => {
                let job_rows = self.jobs.rows(self.active_session_id.as_deref());
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);

                let mut list_items = Vec::new();
                list_items.push(
                    row::with_children(vec![
                        text("Running Background Jobs")
                            .size(16)
                            .width(Length::Fill)
                            .into(),
                        button::text("✕").on_press(Message::CloseDrawer).into(),
                    ])
                    .align_y(Alignment::Center)
                    .into(),
                );

                if job_rows.is_empty() {
                    list_items.push(
                        text("No background subagents or shells running.")
                            .size(13)
                            .into(),
                    );
                } else {
                    for row in job_rows {
                        let kind_str = match row.kind {
                            JobKind::Subagent => "🤖 Subagent",
                            JobKind::Shell => "🐚 Shell",
                        };

                        let item = column::with_children(vec![
                            text(format!("{kind_str}: {}", row.title)).size(14).into(),
                            text(row.subtitle(now)).size(12).into(),
                        ])
                        .spacing(2);

                        list_items.push(container(item).padding(8).into());
                    }
                }

                let list = column::with_children(list_items).spacing(8).padding(16);
                Some(context_drawer(list, Message::CloseDrawer))
            }
            DrawerPage::Sessions => {
                let mut list_items = Vec::new();
                list_items.push(
                    row::with_children(vec![
                        text("Sessions").size(16).width(Length::Fill).into(),
                        button::text("✕").on_press(Message::CloseDrawer).into(),
                    ])
                    .align_y(Alignment::Center)
                    .into(),
                );

                list_items.push(
                    text_input("Search sessions...", &self.search_query)
                        .on_input(Message::SearchInput)
                        .into(),
                );

                let q = self.search_query.to_lowercase();
                let mut filtered_sessions: Vec<_> = self
                    .sessions
                    .values()
                    .filter(|s| s.parent_id.is_none())
                    .filter(|s| q.is_empty() || s.title.to_lowercase().contains(&q))
                    .collect();

                filtered_sessions.sort_by_key(|s| std::cmp::Reverse(s.time.updated));

                for s in filtered_sessions {
                    let s_id = s.id.clone();
                    let item_btn = button::text(&s.title)
                        .on_press(Message::SelectSession(s_id))
                        .width(Length::Fill);

                    list_items.push(item_btn.into());
                }

                let list = column::with_children(list_items).spacing(8).padding(16);
                Some(context_drawer(scrollable(list), Message::CloseDrawer))
            }
            DrawerPage::Settings => {
                let list = column::with_children(vec![
                    row::with_children(vec![
                        text("Connection Settings")
                            .size(16)
                            .width(Length::Fill)
                            .into(),
                        button::text("✕").on_press(Message::CloseDrawer).into(),
                    ])
                    .align_y(Alignment::Center)
                    .into(),
                    text("OpenCode Server URL:").size(13).into(),
                    text_input("https://...", &self.server_url_input)
                        .on_input(Message::SettingsUrlInput)
                        .into(),
                    text("Username:").size(13).into(),
                    text_input("opencode", &self.username_input)
                        .on_input(Message::SettingsUsernameInput)
                        .into(),
                    text("Password:").size(13).into(),
                    text_input("Password", &self.password_input)
                        .on_input(Message::SettingsPasswordInput)
                        .password()
                        .into(),
                    button::text("Save & Connect")
                        .on_press(Message::ApplySettings)
                        .into(),
                ])
                .spacing(12)
                .padding(16);

                Some(context_drawer(list, Message::CloseDrawer))
            }
        }
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        cosmic::iced::time::every(Duration::from_millis(50)).map(|_| Message::Tick)
    }
}

impl OpenCodeCosmic {
    fn next_request_id(&mut self) -> u64 {
        let id = self.next_req_id;
        self.next_req_id += 1;
        id
    }

    fn connect_api(&mut self) {
        let config = ApiConfig {
            base_url: self.server_url_input.clone(),
            username: self.username_input.clone(),
            password: if self.password_input.is_empty() {
                None
            } else {
                Some(self.password_input.clone())
            },
            cloudflare_access: self.args.cf_access_client_id.as_ref().and_then(|id| {
                self.args.cf_access_client_secret.as_ref().map(|secret| {
                    CloudflareAccessCredentials {
                        client_id: id.clone(),
                        client_secret: secret.clone(),
                    }
                })
            }),
        };

        match ApiHandle::start(config) {
            Ok((handle, receiver, _server_key)) => {
                self.api = Some(handle.clone());
                self.receiver = Some(receiver);
                self.connection_status = "Connecting...".to_string();

                handle.send(Command::Bootstrap {
                    sessions: self.tabs.clone(),
                    directories: Vec::new(),
                });
            }
            Err(e) => {
                self.error_banner = Some(format!("Failed to connect: {e}"));
                self.connection_status = "Connection Failed".to_string();
            }
        }
    }

    fn drain_events(&mut self) {
        let mut events = Vec::new();
        if let Some(mock) = &mut self.mock_server {
            events = mock.take_server_events();
        } else if let Some(receiver) = &self.receiver {
            while let Ok(event) = receiver.try_recv() {
                events.push(event);
            }
        }

        for event in events {
            self.handle_ui_event(event);
        }
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Connection { connected, error } => {
                if connected {
                    self.connection_status = "Connected".to_string();
                } else {
                    self.connection_status = error.unwrap_or_else(|| "Disconnected".to_string());
                }
            }
            UiEvent::Bootstrap(Ok(bootstrap)) => {
                for session in bootstrap.sessions {
                    self.sessions.insert(session.id.clone(), session);
                }

                for (id, st) in bootstrap.statuses {
                    if st.is_busy() {
                        self.statuses.insert(id, RunStatus::Busy);
                    } else {
                        self.statuses.insert(id, RunStatus::Idle);
                    }
                }

                if self.tabs.is_empty() {
                    let mut roots: Vec<_> = self
                        .sessions
                        .values()
                        .filter(|s| s.parent_id.is_none())
                        .collect();
                    roots.sort_by_key(|s| std::cmp::Reverse(s.time.updated));
                    for r in roots.iter().take(5) {
                        self.tabs.push(r.id.clone());
                    }
                }

                if self.active_session_id.is_none() {
                    self.active_session_id = self.tabs.first().cloned();
                }

                if let Some(active_id) = &self.active_session_id {
                    let dir = self
                        .sessions
                        .get(active_id)
                        .map(|s| s.directory.clone())
                        .unwrap_or_else(|| "/repo".to_string());

                    if let Some(api) = &self.api {
                        api.send(Command::LoadMessages {
                            session_id: active_id.clone(),
                            cursor: None,
                        });
                        api.send(Command::LoadModels { directory: dir });
                    }
                }
            }
            UiEvent::Bootstrap(Err(err)) => {
                self.error_banner = Some(format!("Bootstrap failed: {err}"));
            }
            UiEvent::MessagesLoaded {
                session_id,
                cursor,
                result: Ok(page),
            } => {
                let conv = self.conversations.entry(session_id).or_default();
                if cursor.is_none() {
                    conv.replace_from_api(&page.messages, page.next_cursor);
                } else {
                    conv.prepend_from_api(&page.messages, page.next_cursor);
                }
            }
            UiEvent::ModelsLoaded {
                directory,
                result: Ok(catalog),
            } => {
                self.catalogs.insert(directory, catalog);
            }
            UiEvent::ServerEvent(envelope) => {
                if let Some((sid, status)) = model::event_run_status(&envelope.payload) {
                    self.statuses.insert(sid, status);
                }

                if let Ok(event) = protocol::Event::deserialize(&envelope.payload) {
                    let kind = protocol::decode_event(&event);

                    if let Some(sid) = kind.session_id() {
                        let conv = self.conversations.entry(sid.to_string()).or_default();
                        conv.apply(&event, &kind);
                    }

                    if let Some(job_evt) =
                        jobs::job_event(&event, &kind, envelope.directory.as_deref())
                    {
                        let roots: Vec<Session> = self.sessions.values().cloned().collect();
                        let ctx = jobs::Context {
                            roots: &roots,
                            directories: &[],
                        };
                        self.jobs.apply_event(job_evt, &ctx);
                    }
                }
            }
            UiEvent::SessionCreated {
                result: Ok(session),
                ..
            } => {
                let id = session.id.clone();
                let dir = session.directory.clone();
                self.sessions.insert(id.clone(), session);
                self.tabs.push(id.clone());
                self.active_session_id = Some(id.clone());

                if let Some(api) = &self.api {
                    api.send(Command::LoadMessages {
                        session_id: id,
                        cursor: None,
                    });
                    api.send(Command::LoadModels { directory: dir });
                }
            }
            UiEvent::PromptAccepted {
                session_id,
                result: Err(e),
                ..
            } => {
                self.error_banner = Some(format!("Prompt rejected for {session_id}: {e}"));
            }
            UiEvent::Aborted { session_id, .. } => {
                self.statuses.insert(session_id.clone(), RunStatus::Idle);
                if let Some(conv) = self.conversations.get_mut(&session_id) {
                    conv.sync_queued(&[]);
                }
            }
            UiEvent::SessionRenamed {
                session_id,
                result: Ok(s),
                ..
            } => {
                self.sessions.insert(session_id, s);
            }
            _ => {}
        }
    }

    fn set_active_session(&mut self, id: &str) {
        self.active_session_id = Some(id.to_string());
        if !self.conversations.contains_key(id)
            && let Some(api) = &self.api
        {
            api.send(Command::LoadMessages {
                session_id: id.to_string(),
                cursor: None,
            });
        }
    }

    fn close_tab(&mut self, id: &str) {
        self.tabs.retain(|t| t != id);
        if self.active_session_id.as_deref() == Some(id) {
            self.active_session_id = self.tabs.first().cloned();
        }
    }

    fn open_session(&mut self, id: &str) {
        if !self.tabs.contains(&id.to_string()) {
            self.tabs.push(id.to_string());
        }
        self.set_active_session(id);
    }

    fn create_session(&mut self) {
        let req_id = self.next_request_id();
        if let Some(api) = &self.api {
            api.send(Command::CreateSession {
                request_id: req_id,
                directory: "/repo".to_string(),
                title: None,
            });
        } else if let Some(mock) = &mut self.mock_server {
            let event = mock.handle(Command::CreateSession {
                request_id: req_id,
                directory: "/repo".to_string(),
                title: Some("New Preview Session".to_string()),
            });
            self.handle_ui_event(event);
        }
    }

    fn send_composer_prompt(&mut self, mode: SendMode) {
        let text = self.composer_text.trim();
        if text.is_empty() {
            return;
        }

        let Some(active_id) = self.active_session_id.clone() else {
            return;
        };

        let prompt = std::mem::take(&mut self.composer_text);
        let req_id = self.next_request_id();
        let msg_id = format!("msg_{}", req_id);

        if let Some(api) = &self.api {
            api.send(Command::SendPrompt {
                request_id: req_id,
                message_id: msg_id,
                session_id: active_id,
                text: prompt,
                attachments: Vec::new(),
                delivery: mode.delivery(),
            });
        } else if let Some(mock) = &mut self.mock_server {
            let event = mock.handle(Command::SendPrompt {
                request_id: req_id,
                message_id: msg_id,
                session_id: active_id,
                text: prompt,
                attachments: Vec::new(),
                delivery: mode.delivery(),
            });
            self.handle_ui_event(event);
        }
    }

    fn stop_active_session(&mut self) {
        let Some(active_id) = self.active_session_id.clone() else {
            return;
        };

        if let Some(api) = &self.api {
            api.send(Command::Abort {
                session_id: active_id,
            });
        } else if let Some(mock) = &mut self.mock_server {
            let event = mock.handle(Command::Abort {
                session_id: active_id,
            });
            self.handle_ui_event(event);
        }
    }

    fn handle_tray_action(&mut self, item_id: &str, action: RowAction) {
        let Some(active_id) = self.active_session_id.clone() else {
            return;
        };

        let current_delivery = self
            .conversations
            .get(&active_id)
            .and_then(|c| c.tray_items().into_iter().find(|t| t.id == item_id))
            .map(|t| t.delivery)
            .unwrap_or(protocol::Delivery::Steer);

        let req = match action {
            RowAction::Switch => {
                let new_del = match current_delivery {
                    protocol::Delivery::Steer => protocol::Delivery::Queue,
                    protocol::Delivery::Queue => protocol::Delivery::Steer,
                    _ => protocol::Delivery::Steer,
                };
                crate::api::InboxRequest::SetDelivery(new_del)
            }
            RowAction::Cancel => crate::api::InboxRequest::Cancel,
        };

        if let Some(api) = &self.api {
            api.send(Command::Inbox {
                session_id: active_id,
                inbox_id: item_id.to_string(),
                request: req,
            });
        } else if let Some(mock) = &mut self.mock_server {
            let event = mock.handle(Command::Inbox {
                session_id: active_id,
                inbox_id: item_id.to_string(),
                request: req,
            });
            self.handle_ui_event(event);
        }
    }

    fn clear_tray(&mut self) {
        let Some(active_id) = &self.active_session_id else {
            return;
        };
        let Some(conv) = self.conversations.get(active_id) else {
            return;
        };

        let items = conv.tray_items();
        for item in items {
            self.handle_tray_action(&item.id, RowAction::Cancel);
        }
    }

    fn switch_model(&mut self, model_id: &str) {
        let Some(active_id) = self.active_session_id.clone() else {
            return;
        };

        let req_id = self.next_request_id();
        if let Some(api) = &self.api {
            api.send(Command::SelectModel {
                request_id: req_id,
                session_id: active_id,
                model: protocol::ModelRef {
                    id: model_id.to_string(),
                    provider_id: "anthropic".to_string(),
                    variant: None,
                },
            });
        }
    }

    fn reconnect_with_settings(&mut self) {
        self.state.connection.server = self.server_url_input.clone();
        self.state.connection.username = self.username_input.clone();
        let _ = self.state.save(&default_path());

        self.connect_api();
        self.active_drawer = None;
    }

    fn is_session_busy(&self, id: &str) -> bool {
        self.statuses.get(id).is_some_and(|s| s.is_busy())
    }

    fn active_session_model_label(&self) -> String {
        self.active_session_id
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .and_then(|s| s.model.as_ref())
            .map(|m| m.id.clone())
            .unwrap_or_else(|| "Default".to_string())
    }

    fn active_context_usage(&self) -> String {
        self.active_session_id
            .as_ref()
            .and_then(|id| self.conversations.get(id))
            .and_then(|c| c.context_tokens())
            .map(|tokens| format!("{tokens} tokens"))
            .unwrap_or_default()
    }

    fn active_jobs_count(&self) -> usize {
        self.jobs.rows(self.active_session_id.as_deref()).len()
    }
}
