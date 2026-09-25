use std::{
    collections::{HashMap, HashSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_channel::Receiver;
use cosmic::app::{ContextDrawer, Core, Task, context_drawer};
use cosmic::iced::{Alignment, Border, Color, Length, Subscription};
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
    sidebar_open: bool,
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
    ToggleSidebar,
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
            sidebar_open: true,
            connection_status: "Connecting...".to_string(),
            error_banner: None,
            server_url_input: server_url,
            username_input: username,
            password_input: flags.password.clone().unwrap_or_default(),
            next_req_id: 1,
        };

        if flags.preview {
            let mut mock = preview::State::new();
            app.connection_status = "Preview (Offline)".to_string();

            let b_event = mock.handle(Command::Bootstrap {
                sessions: Vec::new(),
                directories: Vec::new(),
            });
            app.handle_ui_event(b_event);

            let s_state = preview::server_state();
            app.tabs.clear();
            for tab in &s_state.tabs {
                app.tabs.push(tab.id.clone());
            }
            app.active_session_id = s_state.active;

            for tab in &s_state.tabs {
                let m_event = mock.handle(Command::LoadMessages {
                    session_id: tab.id.clone(),
                    cursor: None,
                });
                app.handle_ui_event(m_event);
            }

            let mod_event = mock.handle(Command::LoadModels {
                directory: "/repo".to_string(),
            });
            app.handle_ui_event(mod_event);

            for event in mock.take_server_events() {
                app.handle_ui_event(event);
            }
            app.mock_server = Some(mock);
        } else {
            app.connect_api();
        }

        if let Some(drawer_name) = &flags.drawer {
            match drawer_name.to_lowercase().as_str() {
                "jobs" => app.active_drawer = Some(DrawerPage::Jobs),
                "sessions" => app.active_drawer = Some(DrawerPage::Sessions),
                "settings" => app.active_drawer = Some(DrawerPage::Settings),
                _ => {}
            }
        }

        (app, Task::none())
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Tick => {
                self.drain_events();
                Task::none()
            }
            Message::ToggleSidebar => {
                self.sidebar_open = !self.sidebar_open;
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
        vec![
            button::text("☰")
                .on_press(Message::ToggleSidebar)
                .padding([3, 8])
                .into(),
            text("OpenCode").size(14).into(),
            text(format!("· {}", self.connection_status))
                .size(12)
                .into(),
        ]
    }

    fn header_center(&self) -> Vec<Element<'_, Self::Message>> {
        Vec::new()
    }

    fn header_end(&self) -> Vec<Element<'_, Self::Message>> {
        vec![
            button::text("Tabs (Ctrl+P)")
                .on_press(Message::ToggleDrawer(DrawerPage::Sessions))
                .padding([3, 8])
                .into(),
            button::text("⚙ Settings")
                .on_press(Message::ToggleDrawer(DrawerPage::Settings))
                .padding([3, 8])
                .into(),
        ]
    }

    fn view(&self) -> Element<'_, Self::Message> {
        // 1. Build Left Sidebar (GTK style)
        let mut sidebar_items = Vec::new();

        let new_session_btn = button::text("+ New session")
            .on_press(Message::NewSession)
            .width(Length::Fill)
            .padding([8, 12]);
        sidebar_items.push(container(new_session_btn).padding([8, 8, 4, 8]).into());

        let mut tab_rows = Vec::new();
        for tab_id in &self.tabs {
            let title = self
                .sessions
                .get(tab_id)
                .map(|s| s.title.as_str())
                .unwrap_or(tab_id.as_str());

            let is_busy = self.is_session_busy(tab_id);
            let is_active = self.active_session_id.as_deref() == Some(tab_id.as_str());

            let display_title = if title.chars().count() > 20 {
                let s: String = title.chars().take(19).collect();
                format!("{s}…")
            } else {
                title.to_string()
            };

            let status_dot = if is_busy {
                "⟳"
            } else if is_active {
                "●"
            } else {
                "○"
            };

            let tab_id_clone = tab_id.clone();
            let close_id = tab_id.clone();

            let tab_btn = button::text(format!("{status_dot}  {display_title}"))
                .on_press(Message::SelectTab(tab_id_clone))
                .width(Length::Fill)
                .padding([6, 10]);

            let close_btn = button::text("✕")
                .on_press(Message::CloseTab(close_id))
                .padding([4, 6]);

            let tab_row = row::with_children(vec![tab_btn.into(), close_btn.into()])
                .align_y(Alignment::Center)
                .spacing(2);

            let tab_card = container(tab_row)
                .width(Length::Fill)
                .padding([1, 4])
                .style(move |_theme| {
                    if is_active {
                        container::Style {
                            background: Some(Color::from_rgb8(0x22, 0x26, 0x2a).into()),
                            border: Border {
                                color: Color::from_rgb8(0x2b, 0x30, 0x34),
                                width: 1.0,
                                radius: 6.0.into(),
                            },
                            ..Default::default()
                        }
                    } else {
                        container::Style {
                            background: None,
                            border: Border::default(),
                            ..Default::default()
                        }
                    }
                });

            tab_rows.push(tab_card.into());
        }

        let tab_list_col = column::with_children(tab_rows).spacing(2);
        let tab_scroll = scrollable(tab_list_col)
            .height(Length::Fill)
            .width(Length::Fill);
        sidebar_items.push(
            container(tab_scroll)
                .padding([4, 4])
                .height(Length::Fill)
                .into(),
        );

        let job_rows = self.jobs.rows(self.active_session_id.as_deref());
        if !job_rows.is_empty() {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);

            let mut jobs_col_items = Vec::new();
            jobs_col_items.push(
                text(format!("BACKGROUND ({})", job_rows.len()))
                    .size(11)
                    .into(),
            );

            for row in job_rows {
                let kind_str = match row.kind {
                    JobKind::Subagent => "◈",
                    JobKind::Shell => ">_",
                };

                let item = column::with_children(vec![
                    text(format!("{kind_str} {}", row.title)).size(12).into(),
                    text(row.subtitle(now)).size(10).into(),
                ])
                .spacing(1);

                let job_card =
                    container(item)
                        .padding([4, 8])
                        .width(Length::Fill)
                        .style(|_theme| container::Style {
                            background: Some(Color::from_rgb8(0x18, 0x1c, 0x21).into()),
                            border: Border {
                                color: Color::from_rgb8(0x28, 0x2c, 0x30),
                                width: 1.0,
                                radius: 4.0.into(),
                            },
                            ..Default::default()
                        });

                jobs_col_items.push(job_card.into());
            }

            let jobs_section =
                container(column::with_children(jobs_col_items).spacing(4)).padding([6, 8]);
            sidebar_items.push(jobs_section.into());
        }

        let footer_buttons = column::with_children(vec![
            button::text("All Sessions (Ctrl+P)")
                .on_press(Message::ToggleDrawer(DrawerPage::Sessions))
                .width(Length::Fill)
                .padding([6, 10])
                .into(),
            button::text("Settings (Ctrl+,)")
                .on_press(Message::ToggleDrawer(DrawerPage::Settings))
                .width(Length::Fill)
                .padding([6, 10])
                .into(),
        ])
        .spacing(4);

        let footer_container = container(footer_buttons).padding([8, 8, 8, 8]);
        sidebar_items.push(footer_container.into());

        let sidebar_column = column::with_children(sidebar_items)
            .width(Length::Fixed(260.0))
            .height(Length::Fill);

        let sidebar = container(sidebar_column)
            .height(Length::Fill)
            .style(|_theme| container::Style {
                background: Some(Color::from_rgb8(0x0d, 0x0f, 0x11).into()),
                border: Border {
                    color: Color::from_rgb8(0x24, 0x28, 0x2c),
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            });

        // 2. Build Main Content Pane
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
            let active_title = self
                .sessions
                .get(active_id)
                .map(|s| s.title.as_str())
                .unwrap_or(active_id.as_str());
            let active_model = self.active_session_model_label();
            let usage = self.active_context_usage();

            let hint_str = if usage.is_empty() {
                format!("Model: {active_model}")
            } else {
                format!("Model: {active_model} · {usage}")
            };

            let title_max = if self.active_drawer.is_some() { 20 } else { 38 };
            let display_title = if active_title.chars().count() > title_max {
                let s: String = active_title.chars().take(title_max - 1).collect();
                format!("{s}…")
            } else {
                active_title.to_string()
            };

            let session_header = container(
                row::with_children(vec![
                    text(display_title).size(15).width(Length::Fill).into(),
                    button::text(hint_str)
                        .on_press(Message::ToggleDrawer(DrawerPage::Settings))
                        .padding([3, 8])
                        .into(),
                ])
                .align_y(Alignment::Center)
                .padding([10, 20]),
            )
            .width(Length::Fill)
            .style(|_theme| container::Style {
                background: Some(Color::from_rgb8(0x13, 0x16, 0x19).into()),
                border: Border {
                    color: Color::from_rgb8(0x23, 0x27, 0x2c),
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            });

            main_items.push(session_header.into());
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
                    let role_text = if is_user { "YOU" } else { "AGENT" };

                    let header_row = row::with_children(vec![
                        text(role_text).size(11).width(Length::Fill).into(),
                    ])
                    .align_y(Alignment::Center);

                    let mut turn_items = Vec::new();
                    turn_items.push(header_row.into());

                    for segment in message.segments() {
                        match segment.kind {
                            model::SegmentKind::Text => {
                                if !segment.text.trim().is_empty() {
                                    turn_items.push(markdown::render_markdown(
                                        &segment.text,
                                        Message::CopyText,
                                    ));
                                }
                            }
                            model::SegmentKind::Reasoning => {
                                if !segment.text.trim().is_empty() {
                                    let reasoning_header = text("◈ Thinking").size(11).into();
                                    let reasoning_body = text(segment.text.trim()).size(13).into();
                                    let reasoning_col = column::with_children(vec![
                                        reasoning_header,
                                        reasoning_body,
                                    ])
                                    .spacing(4);

                                    let reasoning_box = container(reasoning_col)
                                        .padding([8, 12])
                                        .width(Length::Fill)
                                        .style(|_theme| container::Style {
                                            background: Some(
                                                Color::from_rgba8(255, 255, 255, 0.02).into(),
                                            ),
                                            border: Border {
                                                color: Color::from_rgb8(0x6f, 0x77, 0x80),
                                                width: 1.0,
                                                radius: 4.0.into(),
                                            },
                                            text_color: Some(Color::from_rgb8(0x8d, 0x95, 0x9d)),
                                            ..Default::default()
                                        });

                                    turn_items.push(reasoning_box.into());
                                }
                            }
                            model::SegmentKind::Tool => {
                                let (name, status, command, output) = if let Some(tool) =
                                    &segment.tool
                                {
                                    let (status_text, command_text, out_text) = match &tool.state {
                                        protocol::ToolState::Completed {
                                            input, content, ..
                                        } => {
                                            let cmd = input
                                                .get("command")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or(&segment.text);
                                            let out = content.first().and_then(|c| match c {
                                                protocol::ToolContent::Text { text } => {
                                                    Some(text.as_str())
                                                }
                                                _ => None,
                                            });
                                            ("COMPLETED", cmd, out)
                                        }
                                        protocol::ToolState::Running { input, .. } => {
                                            let cmd = input
                                                .get("command")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or(&segment.text);
                                            ("RUNNING", cmd, None)
                                        }
                                        protocol::ToolState::Error { input, .. } => {
                                            let cmd = input
                                                .get("command")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or(&segment.text);
                                            ("ERROR", cmd, None)
                                        }
                                        protocol::ToolState::Streaming { .. }
                                        | protocol::ToolState::Unknown => {
                                            ("PENDING", segment.text.as_str(), None)
                                        }
                                    };
                                    (tool.name.as_str(), status_text, command_text, out_text)
                                } else {
                                    ("tool", "COMPLETED", segment.text.as_str(), None)
                                };

                                let is_completed = status == "COMPLETED";
                                let badge_color = if is_completed {
                                    Color::from_rgb8(0x56, 0xd3, 0x64)
                                } else {
                                    Color::from_rgb8(0xe5, 0xb5, 0x67)
                                };

                                let tool_header = row::with_children(vec![
                                    text(format!("⚙ {}", name.to_uppercase()))
                                        .size(11)
                                        .width(Length::Fill)
                                        .into(),
                                    container(text(status).size(10))
                                        .padding([1, 6])
                                        .style(move |_theme| container::Style {
                                            background: Some(
                                                Color::from_rgba8(255, 255, 255, 0.05).into(),
                                            ),
                                            border: Border {
                                                color: badge_color,
                                                width: 1.0,
                                                radius: 4.0.into(),
                                            },
                                            text_color: Some(badge_color),
                                            ..Default::default()
                                        })
                                        .into(),
                                ])
                                .align_y(Alignment::Center);

                                let cmd_text =
                                    text(command).font(cosmic::iced::Font::MONOSPACE).size(12);

                                let mut tool_box_items = vec![tool_header.into(), cmd_text.into()];

                                if let Some(out) = output {
                                    let out_box = container(
                                        text(out).font(cosmic::iced::Font::MONOSPACE).size(12),
                                    )
                                    .padding([6, 10])
                                    .width(Length::Fill)
                                    .style(|_theme| container::Style {
                                        background: Some(Color::from_rgb8(0x0d, 0x0f, 0x11).into()),
                                        border: Border {
                                            color: Color::from_rgb8(0x24, 0x28, 0x2c),
                                            width: 1.0,
                                            radius: 4.0.into(),
                                        },
                                        text_color: Some(Color::from_rgb8(0x89, 0x91, 0x98)),
                                        ..Default::default()
                                    });
                                    tool_box_items.push(out_box.into());
                                }

                                let tool_col = column::with_children(tool_box_items).spacing(6);

                                let tool_card = container(tool_col)
                                    .padding(10)
                                    .width(Length::Fill)
                                    .style(|_theme| container::Style {
                                        background: Some(Color::from_rgb8(0x14, 0x17, 0x1a).into()),
                                        border: Border {
                                            color: Color::from_rgb8(0x28, 0x2c, 0x30),
                                            width: 1.0,
                                            radius: 6.0.into(),
                                        },
                                        ..Default::default()
                                    });

                                turn_items.push(tool_card.into());
                            }
                            model::SegmentKind::File => {
                                let file_text = text(&segment.text).size(13);
                                turn_items.push(file_text.into());
                            }
                        }
                    }

                    if let Some(error) = message.error() {
                        let err_box = container(text(format!("⚠ Error: {error}")).size(13))
                            .padding([8, 12])
                            .width(Length::Fill)
                            .style(|_theme| container::Style {
                                background: Some(Color::from_rgba8(248, 81, 73, 0.08).into()),
                                border: Border {
                                    color: Color::from_rgba8(248, 81, 73, 0.32),
                                    width: 1.0,
                                    radius: 4.0.into(),
                                },
                                text_color: Some(Color::from_rgb8(248, 81, 73)),
                                ..Default::default()
                            });
                        turn_items.push(err_box.into());
                    }

                    let turn_col = column::with_children(turn_items).spacing(8);

                    let card = container(turn_col)
                        .padding([14, 20])
                        .width(Length::Fill)
                        .style(move |_theme| {
                            if is_user {
                                container::Style {
                                    background: Some(Color::from_rgb8(0x1c, 0x24, 0x2b).into()),
                                    border: Border {
                                        color: Color::from_rgba8(255, 255, 255, 0.08),
                                        width: 1.0,
                                        radius: 8.0.into(),
                                    },
                                    ..Default::default()
                                }
                            } else {
                                container::Style {
                                    background: Some(Color::from_rgb8(0x13, 0x16, 0x19).into()),
                                    border: Border {
                                        color: Color::from_rgba8(255, 255, 255, 0.04),
                                        width: 1.0,
                                        radius: 8.0.into(),
                                    },
                                    ..Default::default()
                                }
                            }
                        });

                    message_elements.push(card.into());
                }
            }

            if is_busy {
                let busy_indicator = row::with_children(vec![
                    text("⟳ OpenCode is thinking...").size(13).into(),
                    button::text("Stop")
                        .padding([3, 8])
                        .on_press(Message::StopSession)
                        .into(),
                ])
                .spacing(10)
                .align_y(Alignment::Center);

                let busy_card =
                    container(busy_indicator)
                        .padding([8, 14])
                        .style(|_theme| container::Style {
                            background: Some(Color::from_rgb8(0x18, 0x1c, 0x21).into()),
                            border: Border {
                                color: Color::from_rgb8(0x30, 0x35, 0x3a),
                                width: 1.0,
                                radius: 6.0.into(),
                            },
                            text_color: Some(Color::from_rgb8(0xe5, 0xb5, 0x67)),
                            ..Default::default()
                        });

                message_elements.push(busy_card.into());
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
                            .padding([2, 6])
                            .on_press(Message::TrayClear)
                            .into(),
                    ])
                    .align_y(Alignment::Center)
                    .into(),
                );

                for item in &tray_items {
                    let delivery_label = match item.delivery {
                        protocol::Delivery::Steer => "STEER",
                        protocol::Delivery::Queue => "QUEUE",
                        _ => "STEER",
                    };

                    let preview_text = if item.text.len() > 60 {
                        format!("{}...", &item.text[..60])
                    } else {
                        item.text.clone()
                    };

                    let badge = container(text(delivery_label).size(10))
                        .padding([2, 6])
                        .style(|_theme| container::Style {
                            background: Some(Color::from_rgb8(0xd2, 0x9b, 0x52).into()),
                            border: Border {
                                radius: 4.0.into(),
                                ..Default::default()
                            },
                            text_color: Some(Color::from_rgb8(0x17, 0x13, 0x0e)),
                            ..Default::default()
                        });

                    let item_row = row::with_children(vec![
                        badge.into(),
                        text(preview_text).size(13).width(Length::Fill).into(),
                        button::text("Switch")
                            .padding([2, 6])
                            .on_press(Message::TrayAction(item.id.clone(), RowAction::Switch))
                            .into(),
                        button::text("✕")
                            .padding([2, 4])
                            .on_press(Message::TrayAction(item.id.clone(), RowAction::Cancel))
                            .into(),
                    ])
                    .spacing(8)
                    .align_y(Alignment::Center);

                    let item_card =
                        container(item_row)
                            .padding([4, 8])
                            .style(|_theme| container::Style {
                                background: Some(Color::from_rgb8(0x18, 0x1c, 0x21).into()),
                                border: Border {
                                    color: Color::from_rgb8(0x28, 0x2c, 0x30),
                                    width: 1.0,
                                    radius: 4.0.into(),
                                },
                                ..Default::default()
                            });

                    tray_rows.push(item_card.into());
                }

                let tray_col = column::with_children(tray_rows).spacing(6);
                let tray_container =
                    container(tray_col)
                        .padding(10)
                        .width(Length::Fill)
                        .style(|_theme| container::Style {
                            background: Some(Color::from_rgb8(0x14, 0x17, 0x1a).into()),
                            border: Border {
                                color: Color::from_rgb8(0x2a, 0x2e, 0x32),
                                width: 1.0,
                                radius: 6.0.into(),
                            },
                            ..Default::default()
                        });

                let tray_outer = container(tray_container).padding([0, 16, 6, 16]);
                main_items.push(tray_outer.into());
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
                    button::suggested("Send")
                        .on_press(Message::SendPrompt(SendMode::Send))
                        .into(),
                ])
                .spacing(6)
            };

            let composer_frame = container(
                row::with_children(vec![
                    text_input("Ask OpenCode...", &self.composer_text)
                        .on_input(Message::ComposerInput)
                        .on_submit(|_| Message::SendPrompt(SendMode::Send))
                        .width(Length::Fill)
                        .into(),
                    send_buttons.into(),
                ])
                .spacing(8)
                .padding([6, 10])
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .style(|_theme| container::Style {
                background: Some(Color::from_rgb8(0x19, 0x1c, 0x1f).into()),
                border: Border {
                    color: Color::from_rgb8(0x30, 0x35, 0x3a),
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            });

            let composer_outer = container(composer_frame).padding([8, 16, 14, 16]);
            main_items.push(composer_outer.into());
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

        let main_pane = column::with_children(main_items)
            .width(Length::Fill)
            .height(Length::Fill);

        let body = if self.sidebar_open {
            row::with_children(vec![sidebar.into(), main_pane.into()])
                .width(Length::Fill)
                .height(Length::Fill)
        } else {
            row::with_children(vec![main_pane.into()])
                .width(Length::Fill)
                .height(Length::Fill)
        };

        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_theme| container::Style {
                background: Some(Color::from_rgb8(0x10, 0x12, 0x14).into()),
                ..Default::default()
            })
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
                list_items.push(text("Running Background Jobs").size(16).into());

                if job_rows.is_empty() {
                    list_items.push(
                        text("No background subagents or shells running.")
                            .size(13)
                            .into(),
                    );
                } else {
                    for row in job_rows {
                        let kind_str = match row.kind {
                            JobKind::Subagent => "◈ Subagent",
                            JobKind::Shell => ">_ Shell",
                        };

                        let item = column::with_children(vec![
                            text(format!("{kind_str}: {}", row.title)).size(13).into(),
                            text(row.subtitle(now)).size(11).into(),
                        ])
                        .spacing(3);

                        let job_card =
                            container(item)
                                .padding([8, 12])
                                .width(Length::Fill)
                                .style(|_theme| container::Style {
                                    background: Some(Color::from_rgb8(0x18, 0x1c, 0x21).into()),
                                    border: Border {
                                        color: Color::from_rgb8(0x28, 0x2c, 0x30),
                                        width: 1.0,
                                        radius: 6.0.into(),
                                    },
                                    ..Default::default()
                                });

                        list_items.push(job_card.into());
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
                for session in &bootstrap.sessions {
                    self.sessions.insert(session.id.clone(), session.clone());
                }

                for (id, st) in &bootstrap.statuses {
                    if st.is_busy() {
                        self.statuses.insert(id.clone(), RunStatus::Busy);
                    } else {
                        self.statuses.insert(id.clone(), RunStatus::Idle);
                    }
                }

                let roots: Vec<Session> = self.sessions.values().cloned().collect();
                let ctx = jobs::Context {
                    roots: &roots,
                    directories: &[],
                };
                let active_statuses: HashSet<String> = bootstrap
                    .statuses
                    .iter()
                    .filter(|(_, st)| st.is_busy())
                    .map(|(id, _)| id.clone())
                    .collect();
                self.jobs
                    .apply_snapshot(Some(&active_statuses), bootstrap.shells, &ctx);

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
        if !self.conversations.contains_key(id) {
            if let Some(api) = &self.api {
                api.send(Command::LoadMessages {
                    session_id: id.to_string(),
                    cursor: None,
                });
            } else if let Some(mock) = &mut self.mock_server {
                let event = mock.handle(Command::LoadMessages {
                    session_id: id.to_string(),
                    cursor: None,
                });
                self.handle_ui_event(event);
            }
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
