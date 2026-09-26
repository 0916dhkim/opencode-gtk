use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_channel::Receiver;
use cosmic::app::{ContextDrawer, Core, Task, context_drawer};
use cosmic::iced::{
    Alignment, Border, Event, Length, Subscription,
    event::listen_with,
    keyboard::{self, Key, Modifiers, key::Named},
};
use cosmic::widget::{button, column, container, row, scrollable, text, text_input};
use cosmic::{Application, ApplicationExt, Element};
use serde::Deserialize;

use crate::{
    Args,
    api::{ApiConfig, ApiHandle, Command, UiEvent},
    credentials::CloudflareAccessCredentials,
    icons,
    jobs::{self, JobKind},
    markdown,
    model::{self, Conversation, ModelCatalog, Role, RunStatus, Session, TrayItem},
    palette,
    persist::{PersistedState, default_path},
    preview, protocol,
    tray::{RowAction, SendMode, enter_mode},
};

/// A session-row drag: `to` follows the row under the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TabDrag {
    from: usize,
    to: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawerPage {
    /// GTK's new-session palette.
    NewSession,
    /// GTK's rename dialog (title + session ID).
    Rename,
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
    /// Set when the active session changes: the next tick focuses the composer.
    focus_composer: bool,
    /// UI zoom, mirroring the GTK client's `zoom_level` (0.7 … 1.75).
    zoom: f32,
    /// Locations the server knows (`project.list`), for GTK's new-session
    /// palette.
    projects: Vec<model::Project>,
    /// The rename palette's title entry.
    rename_input: String,
    /// A tray request (switch/cancel/resume) awaits its answer, so Resume
    /// stays disabled the way GTK's did.
    tray_in_flight: bool,
    /// Open permission requests (bootstrap, reconciliation and live events).
    permissions: Vec<crate::pending::PendingRequest>,
    /// Open forms, with `pending`'s visibility and notice rules.
    forms: crate::pending::Forms,
    /// How far the transcript is scrolled from its start (loading older
    /// history) and from its end (following the run, GTK's sticky prompt).
    transcript_from_top: f32,
    transcript_from_bottom: f32,
    /// The transcript is following the end of the run (the user has not
    /// scrolled up).
    transcript_follow: bool,
    /// An older-history request is in flight.
    history_loading: bool,
    /// The session row a drag started on, and the row it would land on:
    /// GTK's drag-to-reorder.
    tab_drag: Option<TabDrag>,
    /// The session row under the cursor, for GTK's row hover (and its tab
    /// actions, which only show for the active or hovered row).
    hovered_tab: Option<String>,
    /// Sessions whose run finished while they were not active: GTK's unread
    /// marker (a blue dot and title until the session is opened).
    unread: std::collections::HashSet<String>,
    /// The session the rename palette edits (the active one unless a row's
    /// rename action set it).
    rename_target: Option<String>,
    /// Alt is held: GTK swapped every row's status marker for its shortcut
    /// number.
    shortcut_hint: bool,
    /// Files picked with the paperclip for the next prompt.
    pending_attachments: Vec<PathBuf>,
    /// Set while the file dialog runs on its own thread.
    attachment_picker: Option<Receiver<Vec<PathBuf>>>,
}

#[derive(Clone, Debug)]
pub enum Message {
    Tick,
    ToggleSidebar,
    SelectTab(String),
    CloseTab(String),
    NewSession,
    CloseActiveTab,
    CycleTab(i32),
    SelectTabIndex(usize),
    ComposerInput(String),
    SendPrompt(SendMode),
    /// Enter in the composer; the run status decides send/steer/queue.
    ComposerEnter {
        ctrl: bool,
    },
    /// Puts the caret back in the composer (Ctrl+G).
    FocusComposer,
    /// Creates a session in a palette-chosen location.
    CreateSessionIn(String),
    /// Resumes a parked tray (GTK's `queue-tray-resume`).
    ResumeTray,
    /// Answers a permission prompt.
    ReplyPermission {
        request_id: String,
        session_id: String,
        decision: protocol::PermissionDecision,
    },
    /// Cancels the form the notice points at (GTK's `Ctrl+Shift+X`).
    CancelVisibleForm,
    /// Alt was pressed or released: GTK's tab shortcut hint.
    AltHint(bool),
    /// GTK's "Load earlier messages" button.
    LoadOlderHistory,
    /// The cursor entered or left a session row.
    TabHover {
        tab: String,
        hovered: bool,
    },
    /// The left button was released: the dragged row lands, or the row under
    /// the cursor is selected.
    PointerRelease,
    /// A session row started being dragged (GTK reorders tabs by drag).
    TabDragStart(usize),
    /// The cursor is over this session row while dragging.
    TabDragOver(usize),
    /// The transcript scrolled: GTK pinned the current request at its top and
    /// loaded older history when it reached the beginning.
    TranscriptScrolled(cosmic::iced::widget::scrollable::Viewport),
    /// Opens the rename palette for the active session.
    OpenRename,
    /// A row's rename action, for that row's session.
    OpenRenameFor(String),
    RenameInput(String),
    ApplyRename,
    /// Opens the file dialog for the composer's attachments.
    PickAttachments,
    RemoveAttachment(usize),
    ZoomIn,
    ZoomOut,
    ZoomReset,
    StopSession,
    TrayAction(String, RowAction),
    TrayClear,
    CopyText(String),
    ToggleDrawer(DrawerPage),
    CloseDrawer,
    SearchInput(String),
    SelectSession(String),
    SelectModel(String),
    SelectVariant(String),
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
        let zoom = if (0.5..=3.0).contains(&state.zoom_level) {
            state.zoom_level as f32
        } else {
            1.0
        };
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
            focus_composer: false,
            zoom,
            projects: Vec::new(),
            rename_input: String::new(),
            tray_in_flight: false,
            permissions: Vec::new(),
            forms: crate::pending::Forms::default(),
            transcript_from_top: 0.0,
            transcript_from_bottom: 0.0,
            transcript_follow: true,
            history_loading: false,
            tab_drag: None,
            hovered_tab: None,
            unread: std::collections::HashSet::new(),
            rename_target: None,
            shortcut_hint: false,
            pending_attachments: Vec::new(),
            attachment_picker: None,
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
            app.focus_composer = app.active_session_id.is_some();

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
            app.projects = vec![
                model::Project {
                    worktree: "/repo".to_string(),
                    name: Some("opencode".to_string()),
                },
                model::Project {
                    worktree: "/state/workspace".to_string(),
                    name: Some("workspace".to_string()),
                },
                model::Project {
                    worktree: "/state/other".to_string(),
                    name: Some("other".to_string()),
                },
            ];

            // Preview mode is the screenshot/demo surface: show the paperclip
            // chips without a real dialog.
            app.pending_attachments = vec![
                PathBuf::from("/state/home/paperclip-22px.png"),
                PathBuf::from("/state/home/composer-actions-34x32.png"),
            ];
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

        // Without this the compositor shows an empty window title.
        let title = if flags.preview {
            "OpenCode Preview".to_string()
        } else {
            "OpenCode".to_string()
        };
        let title_task = match app.core().main_window_id() {
            Some(id) => app.set_window_title(title, id),
            None => Task::none(),
        };

        (app, title_task)
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Tick => {
                // Focus the composer one tick after the active session changed,
                // so the widget exists by the time the focus operation runs.
                let focus = std::mem::take(&mut self.focus_composer);
                let picked = self.take_picked_attachments();
                if let Some(paths) = picked {
                    match crate::api::check_attachments(&paths) {
                        Ok(()) => {
                            for path in paths {
                                if !self.pending_attachments.contains(&path) {
                                    self.pending_attachments.push(path);
                                }
                            }
                        }
                        Err(error) => self.error_banner = Some(error.to_string()),
                    }
                }
                self.drain_events();
                let mut tasks = Vec::new();
                if focus {
                    tasks.push(cosmic::widget::text_input::focus(composer_id()));
                }
                // GTK's transcript followed the run; the scroll itself reports
                // back through `TranscriptScrolled`.
                if self.transcript_follow {
                    tasks.push(cosmic::iced::widget::scrollable::snap_to(
                        transcript_id(),
                        cosmic::iced::widget::scrollable::RelativeOffset {
                            x: None,
                            y: Some(1.0),
                        },
                    ));
                }
                Task::batch(tasks)
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
                self.active_drawer = Some(DrawerPage::NewSession);
                Task::none()
            }
            Message::CreateSessionIn(directory) => {
                self.active_drawer = None;
                self.create_session(&directory);
                Task::none()
            }
            Message::ResumeTray => {
                self.resume_tray();
                Task::none()
            }
            Message::ReplyPermission {
                request_id,
                session_id,
                decision,
            } => {
                if let Some(api) = &self.api {
                    api.send(Command::ReplyPermission {
                        request_id,
                        session_id,
                        decision,
                    });
                }
                Task::none()
            }
            Message::TabHover { tab, hovered } => {
                if hovered {
                    self.hovered_tab = Some(tab);
                } else if self.hovered_tab.as_deref() == Some(tab.as_str()) {
                    self.hovered_tab = None;
                }
                Task::none()
            }
            Message::TabDragStart(position) => {
                self.tab_drag = Some(TabDrag {
                    from: position,
                    to: position,
                });
                Task::none()
            }
            Message::TabDragOver(position) => {
                if let Some(drag) = &mut self.tab_drag {
                    drag.to = position;
                }
                Task::none()
            }
            Message::PointerRelease => {
                if let Some(drag) = self.tab_drag.take() {
                    reorder_tabs(&mut self.tabs, drag.from, drag.to);
                    self.persist_tabs();
                    return Task::none();
                }
                if let Some(tab) = self.hovered_tab.clone()
                    && self.tabs.contains(&tab)
                {
                    self.set_active_session(&tab);
                }
                Task::none()
            }
            Message::TranscriptScrolled(viewport) => {
                // The transcript is anchored to its end, so the absolute offset
                // is the distance from the end and the reversed one from the
                // start (where older history is).
                self.transcript_from_bottom = viewport.absolute_offset().y;
                self.transcript_from_top = viewport.absolute_offset_reversed().y;
                // Follow the run until the user scrolls up; scrolling back to
                // the end resumes it, the way GTK's transcript behaved.
                self.transcript_follow = self.transcript_from_bottom < 24.0;
                self.load_older_history();
                Task::none()
            }
            Message::CancelVisibleForm => {
                if let Some(target) = self.form_notice().and_then(|notice| notice.cancel)
                    && let Some(api) = &self.api
                {
                    api.send(Command::CancelForm {
                        form_id: target.form_id,
                        session_id: target.session_id,
                        directory: target.directory,
                    });
                }
                Task::none()
            }
            Message::OpenRename => {
                self.rename_target = self.active_session_id.clone();
                self.rename_input = self.active_session_title();
                self.active_drawer = Some(DrawerPage::Rename);
                Task::none()
            }
            Message::OpenRenameFor(id) => {
                self.rename_target = Some(id.clone());
                self.rename_input = self
                    .sessions
                    .get(&id)
                    .map(|session| session.title.clone())
                    .unwrap_or_default();
                self.active_drawer = Some(DrawerPage::Rename);
                Task::none()
            }
            Message::AltHint(alt) => {
                self.shortcut_hint = alt;
                Task::none()
            }
            Message::LoadOlderHistory => {
                self.load_older_history();
                Task::none()
            }
            Message::RenameInput(value) => {
                self.rename_input = value;
                Task::none()
            }
            Message::ApplyRename => {
                self.apply_rename();
                Task::none()
            }
            Message::CloseActiveTab => {
                if let Some(id) = self.active_session_id.clone() {
                    self.close_tab(&id);
                }
                Task::none()
            }
            Message::CycleTab(delta) => {
                self.cycle_tab(delta);
                Task::none()
            }
            Message::SelectTabIndex(index) => {
                if let Some(id) = self.tabs.get(index).cloned() {
                    self.set_active_session(&id);
                }
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
            Message::ComposerEnter { ctrl } => {
                if self.active_drawer == Some(DrawerPage::NewSession) {
                    self.confirm_new_session();
                    return Task::none();
                }
                // Enter steers while a run is active, Ctrl+Enter queues a new turn.
                let busy = self
                    .active_session_id
                    .as_deref()
                    .is_some_and(|id| self.is_session_busy(id));
                self.send_composer_prompt(enter_mode(busy, ctrl));
                Task::none()
            }
            Message::FocusComposer => cosmic::widget::text_input::focus(composer_id()),
            Message::PickAttachments => {
                self.pick_attachments();
                Task::none()
            }
            Message::RemoveAttachment(index) => {
                if index < self.pending_attachments.len() {
                    self.pending_attachments.remove(index);
                }
                Task::none()
            }
            Message::ZoomIn => {
                self.zoom_step(1);
                Task::none()
            }
            Message::ZoomOut => {
                self.zoom_step(-1);
                Task::none()
            }
            Message::ZoomReset => {
                self.set_zoom(1.0);
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
            Message::SelectVariant(variant) => {
                self.switch_variant(&variant);
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
            button::icon(icons::sessions())
                .on_press(Message::ToggleSidebar)
                .into(),
            text("OpenCode")
                .size(self.em(crate::metrics::px(14.0)))
                .into(),
            text("·").size(self.em(crate::metrics::px(12.0))).into(),
            inline_icon(icons::connection(), self.zoom)
                .size(self.em(crate::metrics::px(14.0)) as u16)
                .into(),
            text(&self.connection_status)
                .size(self.em(crate::metrics::px(12.0)))
                .into(),
        ]
    }

    fn header_center(&self) -> Vec<Element<'_, Self::Message>> {
        Vec::new()
    }

    fn header_end(&self) -> Vec<Element<'_, Self::Message>> {
        // GTK's headerbar held only the sidebar toggle and the title; the
        // drawers are reached from the sidebar's footer rows (as in GTK).
        vec![]
    }

    fn view(&self) -> Element<'_, Self::Message> {
        // 1. Build Left Sidebar (GTK style)
        let mut sidebar_items = Vec::new();

        let new_session_btn = button::custom(
            row::with_children(vec![
                inline_icon(icons::add(), self.zoom).into(),
                text("New session")
                    .size(self.em(crate::metrics::px(13.0)))
                    .into(),
            ])
            .spacing(self.space(crate::metrics::px(6.0)))
            .align_y(Alignment::Center),
        )
        .on_press(Message::NewSession)
        .class(flat_button_class(self.zoom))
        .width(Length::Fill)
        .padding([self.pad_px(8.0), self.pad_px(12.0)]);
        sidebar_items.push(
            container(new_session_btn)
                .padding([
                    self.pad_px(8.0),
                    self.pad_px(8.0),
                    self.pad_px(4.0),
                    self.pad_px(8.0),
                ])
                .into(),
        );

        let jobs_sessions = self.jobs.sessions_with_jobs();
        let mut tab_rows = Vec::new();
        let mut first_row = true;
        let mut previous_active = true;
        for (position, tab_id) in self.tabs.iter().enumerate() {
            let title = self
                .sessions
                .get(tab_id)
                .map(|s| s.title.as_str())
                .unwrap_or(tab_id.as_str());

            let is_busy = self.is_session_busy(tab_id);
            let is_active = self.active_session_id.as_deref() == Some(tab_id.as_str());
            let is_unread = self.unread.contains(tab_id);
            // GTK showed the Settings gear while the turn or a job ran, and a
            // coloured dot otherwise; the colour carries the attention.
            let has_jobs = jobs_sessions.contains(tab_id);
            let attention = if is_busy {
                palette::current().status_busy
            } else if is_unread {
                palette::current().status_unread
            } else {
                palette::current().status_idle
            };

            let display_title = truncate_title(title, 28);

            let mut marker_items: Vec<Element<'_, Message>> = Vec::new();
            if is_busy || has_jobs {
                marker_items.push(
                    inline_icon(icons::settings(), self.zoom)
                        .size(self.em(1.04) as u16)
                        .class(cosmic::theme::Svg::custom(move |_theme: &cosmic::Theme| {
                            cosmic::iced::widget::svg::Style {
                                color: Some(attention),
                            }
                        }))
                        .into(),
                );
            } else {
                marker_items.push(status_dot(attention, true));
            }
            // GTK swapped the marker for the row's number while Alt was held.
            let status_marker: Element<'_, Message> = if self.shortcut_hint && position < 9 {
                text((position + 1).to_string())
                    .size(self.em(0.78))
                    .font(cosmic::iced::Font {
                        weight: cosmic::iced::font::Weight::Bold,
                        ..cosmic::iced::Font::DEFAULT
                    })
                    .class(cosmic::theme::Text::Color(
                        palette::current().tab_index_text,
                    ))
                    .into()
            } else {
                row::with_children(marker_items).into()
            };

            // GTK: a finished run or unread output recolours the title (both
            // bold), the active row uses the header title colour, the rest the
            // sidebar's own foreground.
            let title_class = if is_busy {
                cosmic::theme::Text::Color(palette::current().status_busy)
            } else if is_unread {
                cosmic::theme::Text::Color(palette::current().tab_unread_text)
            } else if is_active {
                cosmic::theme::Text::Color(palette::current().header_title_text)
            } else {
                cosmic::theme::Text::Default
            };
            let title_weight = if is_busy || is_unread {
                cosmic::iced::font::Weight::Bold
            } else {
                cosmic::iced::font::Weight::Normal
            };

            let close_id = tab_id.clone();
            let row_hint = self
                .sessions
                .get(tab_id)
                .map(|session| format!("{}\nOpen session", session.directory))
                .unwrap_or_else(|| "Open session".to_string());
            let hovered = self.hovered_tab.as_deref() == Some(tab_id.as_str());
            let show_actions = is_active || hovered || self.tab_drag.is_some();

            let tab_btn = container(
                row::with_children(vec![
                    status_marker,
                    text(display_title)
                        .size(self.em(1.0))
                        .wrapping(cosmic::iced::widget::text::Wrapping::None)
                        .font(cosmic::iced::Font {
                            weight: title_weight,
                            ..cosmic::iced::Font::DEFAULT
                        })
                        .class(title_class)
                        .width(Length::Fill)
                        .into(),
                ])
                .spacing(self.space(0.22))
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([0, self.space(0.59) as u16]);

            // GTK kept rename and close on every row, dimmed until the row is
            // active or hovered.
            let close_btn = button::icon(icons::close())
                .on_press(Message::CloseTab(close_id))
                .padding([self.space(0.2) as u16, self.space(0.4) as u16])
                .class(close_button_class(show_actions, self.space(0.81)));
            let rename_btn = button::icon(icons::edit())
                .padding([self.space(0.2) as u16, self.space(0.2) as u16])
                .on_press(Message::OpenRenameFor(tab_id.clone()))
                .class(tab_action_class(show_actions, self.space(0.81)));

            let close_btn = hinted(close_btn, "Close tab");
            let rename_btn = hinted(rename_btn, "Rename session (F2)");

            let mut tab_row_items = vec![tab_btn.into()];
            tab_row_items.push(rename_btn);
            tab_row_items.push(close_btn);
            let tab_row = row::with_children(tab_row_items)
                .align_y(Alignment::Center)
                .spacing(self.space(0.15));

            let radius = self.space(0.5);
            let dragging = self.tab_drag.is_some_and(|drag| drag.from == position);
            let drag_target = self
                .tab_drag
                .is_some_and(|drag| drag.to == position && drag.from != position);
            let tab_card = container(tab_row)
                .width(Length::Fill)
                .height(Length::Fixed(self.em(2.6) as f32))
                .padding([0, self.space(0.3) as u16])
                .style(move |_theme: &cosmic::Theme| {
                    // GTK marked the dragged row and the drop position.
                    if drag_target {
                        return container::Style {
                            border: Border {
                                color: palette::current().accent_bg,
                                width: 1.0,
                                radius: radius.into(),
                            },
                            ..Default::default()
                        };
                    }
                    if dragging {
                        return container::Style {
                            background: Some(palette::current().sidebar_row_active_bg.into()),
                            border: Border {
                                color: palette::current().accent_bg,
                                width: 1.0,
                                radius: radius.into(),
                            },
                            ..Default::default()
                        };
                    }
                    if is_active || hovered {
                        container::Style {
                            background: Some(
                                if is_active {
                                    palette::current().sidebar_row_active_bg
                                } else {
                                    palette::current().sidebar_hover_bg
                                }
                                .into(),
                            ),
                            ..Default::default()
                        }
                    } else {
                        container::Style::default()
                    }
                });

            // GTK's drag-to-reorder: each row reports the drag and the rows the
            // cursor crosses, so a drop lands where the cursor is.
            let hover_id = tab_id.clone();
            let leave_id = tab_id.clone();
            let tab_card: Element<'_, Message> = tab_card.into();
            let tab_card = cosmic::iced::widget::mouse_area(tab_card)
                .on_enter(Message::TabHover {
                    tab: hover_id,
                    hovered: true,
                })
                .on_exit(Message::TabHover {
                    tab: leave_id,
                    hovered: false,
                })
                .on_drag(Message::TabDragStart(position))
                .on_move(move |_point| Message::TabDragOver(position));
            let tab_card = hinted(tab_card, row_hint);

            // GTK separates inactive rows with a hairline.
            if !first_row && !is_active && !previous_active {
                tab_rows.push(hairline(palette::current().nav_separator));
            }
            tab_rows.push(tab_card);
            previous_active = is_active;
            first_row = false;
        }

        let tab_list_col =
            column::with_children(tab_rows).spacing(self.space(crate::metrics::px(2.0)));
        let tab_scroll = scrollable(tab_list_col)
            .height(Length::Fill)
            .width(Length::Fill);
        sidebar_items.push(
            container(tab_scroll)
                .padding([self.pad_px(4.0), self.pad_px(4.0)])
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
                    .size(self.em(crate::metrics::px(11.0)))
                    .into(),
            );

            for row in job_rows {
                let kind_str = match row.kind {
                    JobKind::Subagent => "◆",
                    JobKind::Shell => "$",
                };

                let item = column::with_children(vec![
                    text(format!("{kind_str} {}", row.title))
                        .size(self.em(crate::metrics::px(12.0)))
                        .into(),
                    text(row.subtitle(now))
                        .size(self.em(crate::metrics::px(10.0)))
                        .into(),
                ])
                .spacing(self.space(crate::metrics::px(1.0)));

                let job_card = container(item)
                    .padding([self.pad_px(4.0), self.pad_px(8.0)])
                    .width(Length::Fill)
                    .style(|_theme| container::Style {
                        background: Some(palette::current().card_bg.into()),
                        border: Border {
                            color: palette::current().panel_border,
                            width: 1.0,
                            radius: 4.0.into(),
                        },
                        ..Default::default()
                    });

                jobs_col_items.push(job_card.into());
            }

            let jobs_section = container(
                column::with_children(jobs_col_items).spacing(self.space(crate::metrics::px(4.0))),
            )
            .padding([self.pad_px(6.0), self.pad_px(8.0)]);
            sidebar_items.push(jobs_section.into());
        }

        let footer_buttons = column::with_children(vec![
            button::custom(hinted(
                row::with_children(vec![
                    inline_icon(icons::sessions(), self.zoom).into(),
                    text("Tabs").size(self.em(crate::metrics::px(13.0))).into(),
                ])
                .spacing(self.space(crate::metrics::px(6.0)))
                .align_y(Alignment::Center),
                "Search tabs (Ctrl+P)",
            ))
            .on_press(Message::ToggleDrawer(DrawerPage::Sessions))
            .class(flat_button_class(self.zoom))
            .width(Length::Fill)
            .padding([
                self.pad_px(11.0),
                self.pad_px(9.0),
                self.pad_px(11.0),
                self.pad_px(8.0),
            ])
            .into(),
            button::custom(hinted(
                row::with_children(vec![
                    inline_icon(icons::settings(), self.zoom).into(),
                    text("Settings")
                        .size(self.em(crate::metrics::px(13.0)))
                        .into(),
                ])
                .spacing(self.space(crate::metrics::px(6.0)))
                .align_y(Alignment::Center),
                "Server connection (Ctrl+,)",
            ))
            .on_press(Message::ToggleDrawer(DrawerPage::Settings))
            .class(flat_button_class(self.zoom))
            .width(Length::Fill)
            .padding([
                self.pad_px(11.0),
                self.pad_px(9.0),
                self.pad_px(11.0),
                self.pad_px(8.0),
            ])
            .into(),
        ])
        .spacing(self.space(crate::metrics::px(4.0)));

        let footer_container = container(footer_buttons).padding([
            self.pad_px(8.0),
            self.pad_px(8.0),
            self.pad_px(8.0),
            self.pad_px(8.0),
        ]);
        sidebar_items.push(footer_container.into());

        // GTK measured ≈272px in the last screenshots.
        let sidebar_column = column::with_children(sidebar_items)
            .width(Length::Fixed(272.0))
            .height(Length::Fill);

        let sidebar = container(sidebar_column)
            .height(Length::Fill)
            .style(|_theme| container::Style {
                background: Some(palette::current().sidebar_bg.into()),
                border: Border {
                    color: palette::current().sidebar_border,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            });

        // 2. Build Main Content Pane
        let mut main_items = Vec::new();

        // The banner and tray nodes stay in the tree even when they are empty:
        // a sibling appearing above the composer would otherwise be matched
        // against the composer's own widget state, dropping its focus while a
        // run starts.
        let banner_node = if let Some(err) = &self.error_banner {
            let banner = row::with_children(vec![
                text(format!("⚠ {err}"))
                    .size(self.em(crate::metrics::px(13.0)))
                    .width(Length::Fill)
                    .into(),
                button::text("Dismiss")
                    .on_press(Message::DismissError)
                    .into(),
            ])
            .padding(8)
            .spacing(self.space(crate::metrics::px(8.0)));

            container(banner).padding(4)
        } else {
            container(column::with_children(Vec::<Element<'_, Message>>::new()))
        };

        main_items.push(banner_node.into());

        if let Some(active_id) = &self.active_session_id {
            let active_title = self
                .sessions
                .get(active_id)
                .map(|s| s.title.as_str())
                .unwrap_or(active_id.as_str());
            // GTK's strip: the session's marker, its title, and the tab hint.
            let strip_busy = self.is_session_busy(active_id);
            let strip_jobs = self.jobs.sessions_with_jobs().contains(active_id);
            let strip_attention = if strip_busy {
                palette::current().status_busy
            } else if self.unread.contains(active_id) {
                palette::current().status_unread
            } else {
                palette::current().status_idle
            };
            let strip_marker: Element<'_, Message> = if strip_busy || strip_jobs {
                inline_icon(icons::settings(), self.zoom)
                    .size(self.em(1.04) as u16)
                    .class(cosmic::theme::Svg::custom(move |_theme: &cosmic::Theme| {
                        cosmic::iced::widget::svg::Style {
                            color: Some(strip_attention),
                        }
                    }))
                    .into()
            } else {
                status_dot(strip_attention, true)
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
                    strip_marker,
                    text(display_title)
                        .size(self.em(0.9))
                        .font(cosmic::iced::Font {
                            weight: cosmic::iced::font::Weight::Semibold,
                            ..cosmic::iced::Font::DEFAULT
                        })
                        .width(Length::Fill)
                        .into(),
                    text("Ctrl+P to switch")
                        .size(self.em(0.8))
                        .class(cosmic::theme::Text::Color(palette::current().time_text))
                        .into(),
                ])
                .align_y(Alignment::Center)
                .padding([self.space(0.35) as u16, self.space(2.0) as u16]),
            )
            .width(Length::Fill)
            .style(|_theme| container::Style {
                background: Some(palette::current().inset_bg.into()),
                border: Border {
                    color: palette::current().inset_border,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            });

            // GTK's strip is only visible with the sidebar folded (the
            // highlighted row carries the session otherwise).
            if !self.sidebar_open {
                main_items.push(session_header.into());
            }
            let conversation = self.conversations.get(active_id);
            let tray_items: Vec<TrayItem> =
                conversation.map(|c| c.tray_items()).unwrap_or_default();
            let is_busy = self.is_session_busy(active_id);

            let mut message_elements = Vec::new();

            if let Some(conv) = conversation {
                // GTK kept a flat "Load earlier messages" above the transcript
                // whenever the server had another page.
                if conv.next_cursor.is_some() {
                    message_elements.push(
                        container(
                            button::text("Load earlier messages")
                                .on_press(Message::LoadOlderHistory)
                                .class(flat_button_class(self.zoom)),
                        )
                        .width(Length::Fill)
                        .center_x(Length::Fill)
                        .padding([self.pad_px(6.0), 0])
                        .into(),
                    );
                }
                for message in &conv.messages {
                    if message.role == Role::User && tray_items.iter().any(|t| t.id == message.id) {
                        continue;
                    }

                    let is_user = message.role == Role::User;
                    let role_text = if is_user { "YOU" } else { "AGENT" };
                    let role_color = if is_user {
                        palette::current().user_role_text
                    } else {
                        palette::current().muted_text
                    };

                    // GTK: `.message-role` 0.76em/700 plus a right-aligned
                    // `.message-time` 0.76em.
                    let header_row = row::with_children(vec![
                        text(role_text)
                            .size(self.em(0.76))
                            .font(cosmic::iced::Font {
                                weight: cosmic::iced::font::Weight::Bold,
                                ..cosmic::iced::Font::DEFAULT
                            })
                            .class(cosmic::theme::Text::Color(role_color))
                            .width(Length::Fill)
                            .into(),
                        text(clock_time(message.created))
                            .size(self.em(0.76))
                            .class(cosmic::theme::Text::Color(palette::current().time_text))
                            .into(),
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
                                        self.zoom,
                                    ));
                                }
                            }
                            model::SegmentKind::Reasoning => {
                                if !segment.text.trim().is_empty() {
                                    let reasoning = column::with_children(vec![
                                        text("Reasoning")
                                            .size(self.em(0.76))
                                            .class(cosmic::theme::Text::Color(
                                                palette::current().reasoning_text,
                                            ))
                                            .into(),
                                        text(segment.text.trim())
                                            .size(self.em(0.92))
                                            .class(cosmic::theme::Text::Color(
                                                palette::current().reasoning_text,
                                            ))
                                            .into(),
                                    ])
                                    .spacing(self.space(0.3));

                                    turn_items.push(reasoning.into());
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

                                let tool_radius = self.space(0.59);
                                let mut tool_box_items = vec![
                                    text(format!("{} · {}", name, status.to_lowercase()))
                                        .size(self.em(0.76))
                                        .class(cosmic::theme::Text::Color(
                                            palette::current().muted_text,
                                        ))
                                        .into(),
                                    text(command)
                                        .font(cosmic::iced::Font::MONOSPACE)
                                        .size(self.em(0.92))
                                        .class(cosmic::theme::Text::Color(
                                            palette::current().code_content_text,
                                        ))
                                        .into(),
                                ];

                                if let Some(out) = output {
                                    tool_box_items.push(
                                        container(
                                            text(out)
                                                .font(cosmic::iced::Font::MONOSPACE)
                                                .size(self.em(0.92))
                                                .class(cosmic::theme::Text::Color(
                                                    palette::current().code_content_text,
                                                )),
                                        )
                                        .padding([self.space(0.52) as u16, self.space(0.74) as u16])
                                        .width(Length::Fill)
                                        .style(move |_theme: &cosmic::Theme| container::Style {
                                            background: Some(
                                                palette::current().code_block_bg.into(),
                                            ),
                                            border: Border {
                                                color: palette::current().code_block_border,
                                                width: 1.0,
                                                radius: tool_radius.into(),
                                            },
                                            ..Default::default()
                                        })
                                        .into(),
                                    );
                                }

                                let tool_block =
                                    column::with_children(tool_box_items).spacing(self.space(0.3));

                                turn_items.push(tool_block.into());
                            }
                            model::SegmentKind::File => {
                                // GTK rendered inline images (`.message-image`):
                                // the model exposes them as `image_url`.
                                let uri = segment
                                    .image_url
                                    .as_deref()
                                    .unwrap_or(segment.text.as_str());
                                if let Some(bytes) = inline_image_bytes(uri) {
                                    let radius = self.space(0.59);
                                    let handle =
                                        cosmic::iced::widget::image::Handle::from_bytes(bytes);
                                    turn_items.push(
                                        container(
                                            cosmic::iced::widget::image(handle)
                                                .content_fit(cosmic::iced::ContentFit::Contain)
                                                .width(Length::Fixed(
                                                    self.space(crate::metrics::px(360.0)),
                                                )),
                                        )
                                        .style(move |_theme: &cosmic::Theme| container::Style {
                                            border: Border {
                                                color: palette::current().message_border,
                                                width: 1.0,
                                                radius: radius.into(),
                                            },
                                            ..Default::default()
                                        })
                                        .into(),
                                    );
                                } else {
                                    turn_items.push(text(&segment.text).size(self.em(0.92)).into());
                                }
                            }
                        }
                    }

                    if let Some(error) = message.error() {
                        // GTK `.message-error-card`: tinted background, 1px
                        // border, a 4px accent bar on the left, a bold header
                        // and a softer body.
                        let accent =
                            container(row::with_children(Vec::<Element<'_, Message>>::new()))
                                .width(Length::Fixed(4.0))
                                .height(Length::Fill)
                                .style(|_theme: &cosmic::Theme| container::Style {
                                    background: Some(palette::current().error_text.into()),
                                    ..Default::default()
                                });

                        let body = column::with_children(vec![
                            text("⚠ Error")
                                .size(self.em(0.88))
                                .font(cosmic::iced::Font {
                                    weight: cosmic::iced::font::Weight::Bold,
                                    ..cosmic::iced::Font::DEFAULT
                                })
                                .class(cosmic::theme::Text::Color(palette::current().error_text))
                                .into(),
                            text(error)
                                .size(self.em(0.92))
                                .class(cosmic::theme::Text::Color(
                                    palette::current().error_body_text,
                                ))
                                .into(),
                        ])
                        .spacing(self.space(0.3))
                        .width(Length::Fill);

                        let card_radius = self.space(0.44);
                        let card = container(
                            row::with_children(vec![accent.into(), body.into()])
                                .spacing(self.space(1.04))
                                .align_y(Alignment::Start),
                        )
                        .padding(0)
                        .width(Length::Fill)
                        .style(move |_theme: &cosmic::Theme| container::Style {
                            background: Some(palette::current().error_card_bg.into()),
                            border: Border {
                                color: palette::current().error_card_border,
                                width: 1.0,
                                radius: card_radius.into(),
                            },
                            ..Default::default()
                        });

                        turn_items.push(card.into());
                    }

                    let turn_col = column::with_children(turn_items).spacing(self.space(0.59));

                    // GTK: `.message-row { padding: 1.33em 2.07em 1.48em }`
                    // with a hairline bottom border; user turns are full-width
                    // tinted bands.
                    let row = container(turn_col)
                        .padding([
                            self.space(1.33) as u16,
                            self.space(2.07) as u16,
                            self.space(1.48) as u16,
                            self.space(2.07) as u16,
                        ])
                        .width(Length::Fill)
                        .style(move |_theme: &cosmic::Theme| {
                            if is_user {
                                container::Style {
                                    background: Some(palette::current().user_message_bg.into()),
                                    ..Default::default()
                                }
                            } else {
                                container::Style::default()
                            }
                        });

                    message_elements.push(row.into());
                    message_elements.push(hairline(palette::current().message_border));
                }
            }

            // GTK's permission prompts: "Allow {action}?" with the source and
            // metadata lines and Deny / Allow once / Allow always.
            for request in self.visible_permissions() {
                let action = if request.action.trim().is_empty() {
                    "this action".to_owned()
                } else {
                    request.action.clone()
                };
                let mut card_items: Vec<Element<'_, Message>> = vec![
                    text(format!("Allow {action}?"))
                        .size(self.em(0.96))
                        .font(cosmic::iced::Font {
                            weight: cosmic::iced::font::Weight::Bold,
                            ..cosmic::iced::Font::DEFAULT
                        })
                        .class(cosmic::theme::Text::Color(
                            palette::current().prompt_subheading,
                        ))
                        .into(),
                ];
                if let Some(source) = crate::pending::source_text(request) {
                    card_items.push(
                        text(source)
                            .size(self.em(0.9))
                            .class(cosmic::theme::Text::Color(
                                palette::current().prompt_metadata,
                            ))
                            .into(),
                    );
                }
                if let Some(metadata) = crate::pending::metadata_text(request.metadata.as_ref()) {
                    // GTK capped the details block at 320px.
                    card_items.push(
                        container(
                            scrollable(text(metadata).size(self.em(0.9)).class(
                                cosmic::theme::Text::Color(palette::current().prompt_metadata),
                            ))
                            .height(Length::Shrink),
                        )
                        .max_height(self.space(crate::metrics::px(320.0)))
                        .into(),
                    );
                }

                // GTK: right-aligned Deny / Allow once (`suggested-action`) /
                // Always allow. No reply is ever the default, so a stray Enter
                // or Space while typing can never answer a prompt.
                let mut actions: Vec<Element<'_, Message>> = vec![
                    container(row::with_children(Vec::<Element<'_, Message>>::new()))
                        .width(Length::Fill)
                        .into(),
                    tray_text_button(
                        "Deny",
                        self.zoom,
                        Message::ReplyPermission {
                            request_id: request.id.clone(),
                            session_id: request.session_id.clone(),
                            decision: protocol::PermissionDecision::Reject,
                        },
                    ),
                ];
                actions.push(
                    button::custom(
                        text("Allow once")
                            .size(self.em(0.96))
                            .class(cosmic::theme::Text::Color(palette::current().accent_fg)),
                    )
                    .padding([self.space(0.3) as u16, self.space(0.85) as u16])
                    .class(resume_button_class(self.space(0.59)))
                    .on_press(Message::ReplyPermission {
                        request_id: request.id.clone(),
                        session_id: request.session_id.clone(),
                        decision: protocol::PermissionDecision::Once,
                    })
                    .into(),
                );
                if request.offers_always() {
                    actions.push(tray_text_button(
                        "Always allow",
                        self.zoom,
                        Message::ReplyPermission {
                            request_id: request.id.clone(),
                            session_id: request.session_id.clone(),
                            decision: protocol::PermissionDecision::Always,
                        },
                    ));
                }
                card_items.push(
                    row::with_children(actions)
                        .spacing(self.space(0.59))
                        .align_y(Alignment::Center)
                        .into(),
                );

                let radius = self.space(0.67);
                message_elements.push(
                    container(column::with_children(card_items).spacing(self.space(0.44)))
                        .padding([self.space(0.74) as u16, self.space(0.89) as u16])
                        .width(Length::Fill)
                        .style(move |_theme: &cosmic::Theme| container::Style {
                            background: Some(palette::current().form_notice_bg.into()),
                            border: Border {
                                color: palette::current().form_notice_border,
                                width: 1.0,
                                radius: radius.into(),
                            },
                            ..Default::default()
                        })
                        .into(),
                );
            }

            let message_list = column::with_children(message_elements)
                .spacing(self.space(crate::metrics::px(0.0)));

            // Anchored to the end: the run stays in view and `snap_to` keeps
            // it there, while an empty transcript has nothing to anchor.
            let transcript_scroll = scrollable(message_list)
                .id(transcript_id())
                .anchor_bottom()
                .width(Length::Fill)
                .height(Length::Fill)
                .on_scroll(Message::TranscriptScrolled);

            // GTK's `.sticky-message`: the current request stays pinned at the
            // transcript's top (an overlay with a shadow) once it is scrolled.
            let transcript_area: Element<'_, Message> = match self.sticky_prompt() {
                None => transcript_scroll.into(),
                Some((_id, prompt_text)) => {
                    let sticky = container(
                        row::with_children(vec![
                            text("YOU")
                                .size(self.em(0.76))
                                .font(cosmic::iced::Font {
                                    weight: cosmic::iced::font::Weight::Bold,
                                    ..cosmic::iced::Font::DEFAULT
                                })
                                .class(cosmic::theme::Text::Color(
                                    palette::current().user_role_text,
                                ))
                                .into(),
                            text(prompt_text)
                                .size(self.em(0.96))
                                .class(cosmic::theme::Text::Color(palette::current().content_text))
                                .width(Length::Fill)
                                .into(),
                        ])
                        .spacing(self.space(0.59))
                        .align_y(Alignment::Center),
                    )
                    .padding([self.space(0.59) as u16, self.space(2.07) as u16])
                    .width(Length::Fill)
                    .style(move |_theme: &cosmic::Theme| container::Style {
                        background: Some(palette::current().window_bg.into()),
                        border: Border {
                            color: palette::current().sticky_border,
                            width: 1.0,
                            radius: 0.0.into(),
                        },
                        ..Default::default()
                    });

                    cosmic::iced::widget::Stack::new()
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .push(transcript_scroll)
                        .push(container(sticky).align_top(Length::Fill))
                        .into()
                }
            };

            main_items.push(transcript_area);

            // GTK put the working/retry state in a compact pill *below* the
            // transcript (`.transcript-status-compact`), not inside it.
            if let Some(pill) = self.status_pill(active_id) {
                main_items.push(pill);
            }

            // Steer/Queue Tray. Pushed even when it has no rows, so the
            // composer keeps its widget state (and the caret) when a run
            // starts and the tray fills up.
            // GTK's `.form-notice`: one line, the label bold, Cancel (or the
            // hint while every waiting form belongs to another session).
            let notice = self.form_notice();
            let notice_outer: Element<'_, Message> = match notice {
                None => container(column::with_children(Vec::<Element<'_, Message>>::new()))
                    .padding([0, self.pad_px(16.0)])
                    .into(),
                Some(notice) => {
                    let radius = self.space(0.67);
                    let mut items: Vec<Element<'_, Message>> = vec![
                        text(notice.text)
                            .size(self.em(0.9))
                            .font(cosmic::iced::Font {
                                weight: cosmic::iced::font::Weight::Semibold,
                                ..cosmic::iced::Font::DEFAULT
                            })
                            .class(cosmic::theme::Text::Color(
                                palette::current().prompt_subheading,
                            ))
                            .into(),
                    ];
                    match notice.cancel {
                        Some(target) => {
                            items.push(
                                text(crate::pending::CANCEL_FORM_SHORTCUT)
                                    .size(self.em(0.82))
                                    .class(cosmic::theme::Text::Color(
                                        palette::current().prompt_metadata,
                                    ))
                                    .into(),
                            );
                            let _ = target;
                            items.push(tray_text_button(
                                "Cancel",
                                self.zoom,
                                Message::CancelVisibleForm,
                            ));
                        }
                        None => items.push(
                            text(notice.tooltip)
                                .size(self.em(0.82))
                                .class(cosmic::theme::Text::Color(
                                    palette::current().prompt_metadata,
                                ))
                                .into(),
                        ),
                    }
                    container(
                        row::with_children(items)
                            .spacing(self.space(0.59))
                            .align_y(Alignment::Center),
                    )
                    .padding([self.space(0.59) as u16, self.space(1.23) as u16])
                    .width(Length::Fill)
                    .style(move |_theme: &cosmic::Theme| container::Style {
                        background: Some(palette::current().form_notice_bg.into()),
                        border: Border {
                            color: palette::current().form_notice_border,
                            width: 1.0,
                            radius: radius.into(),
                        },
                        ..Default::default()
                    })
                    .into()
                }
            };

            let tray_outer = if tray_items.is_empty() {
                container(column::with_children(Vec::<Element<'_, Message>>::new())).padding([
                    self.pad_px(0.0),
                    self.pad_px(16.0),
                    self.pad_px(0.0),
                    self.pad_px(16.0),
                ])
            } else {
                // GTK: `.queue-tray-header` with a bold, padded title, then
                // hairline-separated rows.
                // GTK: "3 waiting" while the run is active, "Paused · 3 waiting"
                // with a dot and Resume once it is idle with items left.
                let paused = !is_busy;
                let mut header_items: Vec<Element<'_, Message>> = Vec::new();
                if paused {
                    let dot_radius = self.space(0.12);
                    header_items.push(
                        container(row::with_children(Vec::<Element<'_, Message>>::new()))
                            .width(Length::Fixed(self.space(0.55)))
                            .height(Length::Fixed(self.space(0.55)))
                            .style(move |_theme: &cosmic::Theme| container::Style {
                                background: Some(palette::current().tray_paused.into()),
                                border: Border {
                                    radius: dot_radius.into(),
                                    ..Default::default()
                                },
                                ..Default::default()
                            })
                            .into(),
                    );
                }
                header_items.push(
                    text(crate::tray::header_text(tray_items.len(), paused))
                        .size(self.em(0.96))
                        .font(cosmic::iced::Font {
                            weight: cosmic::iced::font::Weight::Bold,
                            ..cosmic::iced::Font::DEFAULT
                        })
                        .class(cosmic::theme::Text::Color(
                            palette::current().tray_title_text,
                        ))
                        .width(Length::Fill)
                        .into(),
                );
                if paused {
                    let resume_radius = self.space(0.59);
                    header_items.push(
                        button::custom(
                            text("Resume")
                                .size(self.em(0.96))
                                .class(cosmic::theme::Text::Color(palette::current().accent_fg)),
                        )
                        .padding([self.space(0.3) as u16, self.space(0.85) as u16])
                        .class(resume_button_class(resume_radius))
                        .on_press_maybe((!self.tray_in_flight).then_some(Message::ResumeTray))
                        .into(),
                    );
                }
                header_items.push(tray_text_button("Clear all", self.zoom, Message::TrayClear));

                let mut tray_rows = Vec::new();
                tray_rows.push(
                    container(
                        row::with_children(header_items)
                            .spacing(self.space(0.44))
                            .align_y(Alignment::Center),
                    )
                    .padding([self.space(0.1) as u16, self.space(0.6) as u16])
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

                    // GTK queue badges: tinted pills, amber for steers and
                    // grey for queued turns.
                    let is_steer = item.delivery != protocol::Delivery::Queue;
                    let (badge_bg, badge_border, badge_fg) = if is_steer {
                        (
                            palette::current().badge_steer_bg,
                            palette::current().badge_steer_border,
                            palette::current().badge_steer_text,
                        )
                    } else {
                        (
                            palette::current().badge_queue_bg,
                            palette::current().badge_queue_border,
                            palette::current().badge_queue_text,
                        )
                    };

                    let badge_radius = self.space(0.44);
                    let badge = container(text(delivery_label).size(self.em(0.72)).font(
                        cosmic::iced::Font {
                            weight: cosmic::iced::font::Weight::Bold,
                            ..cosmic::iced::Font::DEFAULT
                        },
                    ))
                    .padding([self.space(0.15) as u16, self.space(0.44) as u16])
                    .style(move |_theme: &cosmic::Theme| container::Style {
                        background: Some(badge_bg.into()),
                        border: Border {
                            color: badge_border,
                            width: 1.0,
                            radius: badge_radius.into(),
                        },
                        text_color: Some(badge_fg),
                        ..Default::default()
                    });

                    let mut row_items: Vec<Element<'_, Message>> = vec![
                        badge.into(),
                        text(preview_text)
                            .size(self.em(0.96))
                            .class(cosmic::theme::Text::Color(palette::current().tray_text))
                            .width(Length::Fill)
                            .into(),
                    ];
                    // GTK hides a queued row's switch while the tray is paused.
                    if crate::tray::shows_switch(item.delivery, paused) {
                        row_items.push(tray_text_button(
                            crate::tray::switch_label(item.delivery),
                            self.zoom,
                            Message::TrayAction(item.id.clone(), RowAction::Switch),
                        ));
                    }
                    row_items.push(tray_icon_button(
                        icons::close(),
                        self.zoom,
                        Message::TrayAction(item.id.clone(), RowAction::Cancel),
                    ));
                    let item_row = row::with_children(row_items)
                        .spacing(self.space(0.59))
                        .align_y(Alignment::Center);

                    tray_rows.push(
                        container(item_row)
                            .padding([self.space(0.3) as u16, self.space(0.6) as u16])
                            .into(),
                    );
                    tray_rows.push(hairline(palette::current().tray_row_divider));
                }

                let tray_radius = self.space(0.67);
                let tray_col =
                    column::with_children(tray_rows).spacing(self.space(crate::metrics::px(0.0)));
                let tray_container = container(tray_col)
                    .padding([
                        self.space(0.25) as u16,
                        self.space(0.3) as u16,
                        self.space(0.3) as u16,
                        self.space(0.3) as u16,
                    ])
                    .width(Length::Fill)
                    .style(move |_theme: &cosmic::Theme| container::Style {
                        background: Some(palette::current().tray_bg.into()),
                        border: Border {
                            color: palette::current().tray_border,
                            width: 1.0,
                            radius: tray_radius.into(),
                        },
                        ..Default::default()
                    });

                container(tray_container).padding([
                    self.pad_px(0.0),
                    self.pad_px(16.0),
                    self.pad_px(6.0),
                    self.pad_px(16.0),
                ])
            };

            main_items.push(notice_outer);
            main_items.push(tray_outer.into());

            // Composer area
            // GTK's composer: the prompt row, then a footer with the model
            // menu, the token counter, the queue hint while a run is active,
            // and the actions (0.3/0.59/1.19em group spacing).
            let active_dir = self
                .active_session_id
                .as_ref()
                .and_then(|id| self.sessions.get(id))
                .map(|s| s.directory.clone());
            let catalog = active_dir.as_ref().and_then(|d| self.catalogs.get(d));
            let model_id = self.active_session_model_id();

            let supports_attachments = catalog.is_some_and(|catalog| {
                catalog
                    .models
                    .iter()
                    .any(|model| model.supports_attachments)
            });

            let mut footer_items: Vec<Element<'_, Message>> = Vec::new();

            if supports_attachments {
                footer_items.push(
                    button::icon(icons::attach())
                        .padding([self.space(0.2) as u16, self.space(0.3) as u16])
                        .on_press(Message::PickAttachments)
                        .into(),
                );
            }

            if let Some(catalog) = catalog
                && !catalog.models.is_empty()
            {
                let labels: Vec<String> = catalog.models.iter().map(|m| m.label.clone()).collect();
                let ids: Vec<String> = catalog.models.iter().map(|m| m.model_id.clone()).collect();
                let selected = model_id
                    .as_ref()
                    .and_then(|id| ids.iter().position(|candidate| candidate == id));
                footer_items.push(
                    row::with_children(vec![
                        cosmic::widget::dropdown::dropdown(labels, selected, move |index| {
                            Message::SelectModel(ids.get(index).cloned().unwrap_or_default())
                        })
                        .width(Length::Shrink)
                        .into(),
                        menu_chevron(self.zoom),
                    ])
                    .spacing(self.space(0.3))
                    .align_y(Alignment::Center)
                    .into(),
                );

                if let Some(model) = model_id
                    .as_ref()
                    .and_then(|id| catalog.models.iter().find(|model| &model.model_id == id))
                    .filter(|model| !model.variants.is_empty())
                {
                    let mut labels = vec!["Default".to_string()];
                    labels.extend(model.variants.iter().cloned());
                    let mut variants = vec![String::new()];
                    variants.extend(model.variants.iter().cloned());
                    let selected = self
                        .sessions
                        .get(active_id)
                        .and_then(|session| session.model.as_ref())
                        .and_then(|model| model.variant.as_ref())
                        .and_then(|variant| model.variants.iter().position(|v| v == variant))
                        .map(|index| index + 1)
                        .unwrap_or(0);
                    footer_items.push(
                        row::with_children(vec![
                            cosmic::widget::dropdown::dropdown(
                                labels,
                                Some(selected),
                                move |index| {
                                    Message::SelectVariant(
                                        variants.get(index).cloned().unwrap_or_default(),
                                    )
                                },
                            )
                            .width(Length::Shrink)
                            .into(),
                            menu_chevron(self.zoom),
                        ])
                        .spacing(self.space(0.3))
                        .align_y(Alignment::Center)
                        .into(),
                    );
                }
            }

            footer_items.push(
                row::with_children(Vec::<Element<'_, Message>>::new())
                    .width(Length::Fill)
                    .into(),
            );

            let usage = self.active_context_usage();
            if !usage.is_empty() {
                footer_items.push(
                    text(usage)
                        .size(self.em(0.82))
                        .class(cosmic::theme::Text::Color(palette::current().muted_text))
                        .into(),
                );
            }
            if is_busy {
                footer_items.push(
                    text("Ctrl + Enter to queue")
                        .size(self.em(0.82))
                        .class(cosmic::theme::Text::Color(palette::current().muted_text))
                        .into(),
                );
            }

            let mut action_items: Vec<Element<'_, Message>> = Vec::new();
            if is_busy {
                action_items.push(
                    button::icon(icons::stop())
                        .on_press(Message::StopSession)
                        .into(),
                );
            }
            action_items.push(
                button::icon(icons::send())
                    .class(accent_button_class(self.zoom))
                    .on_press(Message::SendPrompt(SendMode::Send))
                    .into(),
            );

            let mut composer_items: Vec<Element<'_, Message>> = Vec::new();

            // The chip row stays in the tree even when empty: dropping it
            // would shift the prompt input's widget state (iced matches
            // siblings positionally) and panic on the next frame.
            let chip_radius = self.space(0.44);
            let chips: Vec<Element<'_, Message>> = self
                .pending_attachments
                .iter()
                .enumerate()
                .map(|(index, path)| {
                    container(
                        row::with_children(vec![
                            inline_icon(icons::attach(), self.zoom)
                                .size(self.em(0.76) as u16)
                                .into(),
                            text(attachment_label(path))
                                .size(self.em(0.88))
                                .class(cosmic::theme::Text::Color(palette::current().tray_text))
                                .into(),
                            button::icon(icons::close())
                                .padding([self.pad_px(1.0), self.pad_px(3.0)])
                                .on_press(Message::RemoveAttachment(index))
                                .into(),
                        ])
                        .spacing(self.space(0.3))
                        .align_y(Alignment::Center),
                    )
                    .padding([self.space(0.15) as u16, self.space(0.44) as u16])
                    .style(move |_theme: &cosmic::Theme| container::Style {
                        background: Some(palette::current().card_bg.into()),
                        border: Border {
                            color: palette::current().panel_border,
                            width: 1.0,
                            radius: chip_radius.into(),
                        },
                        ..Default::default()
                    })
                    .into()
                })
                .collect();
            composer_items.push(
                row::with_children(chips)
                    .spacing(self.space(0.3))
                    .wrap()
                    .into(),
            );

            composer_items.push(
                text_input("Ask OpenCode...", &self.composer_text)
                    .id(composer_id())
                    .on_input(Message::ComposerInput)
                    // Enter / Ctrl+Enter are handled by the key subscription (see `shortcut`),
                    // so the widget must not also submit on Enter.
                    .width(Length::Fill)
                    .into(),
            );

            let footer = row::with_children(
                footer_items
                    .into_iter()
                    .chain(action_items)
                    .collect::<Vec<_>>(),
            )
            .spacing(self.space(0.59))
            .align_y(Alignment::Center);

            composer_items.push(footer.into());

            let composer_radius = self.space(0.89);
            let composer_frame = container(
                column::with_children(composer_items)
                    .spacing(self.space(0.3))
                    .width(Length::Fill),
            )
            .padding([self.space(0.59) as u16, self.space(0.74) as u16])
            .width(Length::Fill)
            .style(move |_theme: &cosmic::Theme| container::Style {
                background: Some(palette::current().composer_bg.into()),
                border: Border {
                    color: palette::current().composer_border,
                    width: 1.0,
                    radius: composer_radius.into(),
                },
                ..Default::default()
            });

            let composer_outer = container(composer_frame).padding([
                self.space(0.59) as u16,
                self.space(1.19) as u16,
                self.space(1.19) as u16,
                self.space(1.19) as u16,
            ]);
            main_items.push(composer_outer.into());
        } else {
            let empty_view = column::with_children(vec![
                text("Welcome to OpenCode COSMIC")
                    .size(self.em(crate::metrics::px(20.0)))
                    .into(),
                text(format!("Status: {}", self.connection_status))
                    .size(self.em(crate::metrics::px(14.0)))
                    .into(),
                button::text("Create New Session")
                    .on_press(Message::NewSession)
                    .padding([self.pad_px(8.0), self.pad_px(16.0)])
                    .into(),
            ])
            .spacing(self.space(crate::metrics::px(16.0)))
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
                background: Some(palette::current().window_bg.into()),
                ..Default::default()
            })
            .into()
    }

    fn context_drawer(&self) -> Option<ContextDrawer<'_, Self::Message>> {
        // GTK had no side panels: sessions and settings were centred modal
        // palettes, and only the background-job list stays a drawer here.
        if self.active_drawer != Some(DrawerPage::Jobs) {
            return None;
        }

        let job_rows = self.jobs.rows(self.active_session_id.as_deref());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mut list_items = Vec::new();
        list_items.push(text("Running Background Jobs").size(self.em(1.14)).into());

        if job_rows.is_empty() {
            list_items.push(
                text("No background subagents or shells running.")
                    .size(self.em(0.96))
                    .into(),
            );
        } else {
            for row in job_rows {
                let kind_str = match row.kind {
                    JobKind::Subagent => "◆ Subagent",
                    JobKind::Shell => "$ Shell",
                };

                let item = column::with_children(vec![
                    text(format!("{kind_str}: {}", row.title))
                        .size(self.em(0.96))
                        .into(),
                    text(row.subtitle(now))
                        .size(self.em(0.82))
                        .class(cosmic::theme::Text::Color(palette::current().muted_text))
                        .into(),
                ])
                .spacing(self.space(0.3));

                let radius = self.space(0.44);
                let job_card = container(item)
                    .padding([self.space(0.59) as u16, self.space(0.89) as u16])
                    .width(Length::Fill)
                    .style(move |_theme: &cosmic::Theme| container::Style {
                        background: Some(palette::current().card_bg.into()),
                        border: Border {
                            color: palette::current().panel_border,
                            width: 1.0,
                            radius: radius.into(),
                        },
                        ..Default::default()
                    });

                list_items.push(job_card.into());
            }
        }

        let list = column::with_children(list_items)
            .spacing(self.space(0.59))
            .padding(self.space(1.19) as u16);
        Some(context_drawer(list, Message::CloseDrawer))
    }

    /// GTK's centred modal palettes (`.app-modal-palette`).
    fn dialog(&self) -> Option<Element<'_, Self::Message>> {
        match self.active_drawer? {
            DrawerPage::Jobs => None,
            DrawerPage::Sessions => Some(self.sessions_palette()),
            DrawerPage::Settings => Some(self.settings_palette()),
            DrawerPage::NewSession => Some(self.new_session_palette()),
            DrawerPage::Rename => Some(self.rename_palette()),
        }
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::batch([
            cosmic::iced::time::every(Duration::from_millis(50)).map(|_| Message::Tick),
            listen_with(|event, _status, _window| match event {
                Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    shortcut(&key, modifiers)
                }
                // GTK showed each row's shortcut number while Alt was held.
                Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                    Some(Message::AltHint(modifiers.alt()))
                }
                // A row's press must not rebuild it (the drag state lives in
                // the widget), so selection and the drop both happen here: the
                // release either lands a dragged row or selects the row under
                // the cursor.
                Event::Mouse(cosmic::iced::mouse::Event::ButtonReleased(
                    cosmic::iced::mouse::Button::Left,
                )) => Some(Message::PointerRelease),
                _ => None,
            }),
        ])
    }
}

/// Keyboard shortcuts, mirroring the GTK client's key controller.
///
/// Shortcuts advertised in the UI ("Ctrl+P", "Ctrl+,", "Ctrl+Enter") must
/// resolve here. Enter reports whether Ctrl is held; the run status then
/// decides between send, steer and queue (see [`enter_mode`]).
fn shortcut(key: &Key, modifiers: Modifiers) -> Option<Message> {
    if modifiers.control() {
        return match key {
            Key::Character(c) => match c.as_str() {
                // GTK's `crate::pending::CANCEL_FORM_SHORTCUT`.
                "x" | "X" if modifiers.shift() => Some(Message::CancelVisibleForm),
                "t" | "T" => Some(Message::NewSession),
                "b" | "B" => Some(Message::ToggleSidebar),
                "w" | "W" => Some(Message::CloseActiveTab),
                "p" | "P" => Some(Message::ToggleDrawer(DrawerPage::Sessions)),
                "," => Some(Message::ToggleDrawer(DrawerPage::Settings)),
                "g" | "G" => Some(Message::FocusComposer),
                "=" | "+" => Some(Message::ZoomIn),
                "-" => Some(Message::ZoomOut),
                "0" => Some(Message::ZoomReset),
                _ => tab_index(c).map(Message::SelectTabIndex),
            },
            Key::Named(Named::Enter) => Some(Message::ComposerEnter { ctrl: true }),
            Key::Named(Named::Tab) => {
                Some(Message::CycleTab(if modifiers.shift() { -1 } else { 1 }))
            }
            _ => None,
        };
    }

    if modifiers.alt() {
        return match key {
            Key::Character(c) => tab_index(c).map(Message::SelectTabIndex),
            _ => None,
        };
    }

    match key {
        Key::Named(Named::F2) => Some(Message::OpenRename),
        Key::Named(Named::Enter) => Some(Message::ComposerEnter { ctrl: false }),
        Key::Named(Named::Escape) => Some(Message::CloseDrawer),
        _ => None,
    }
}

/// GTK restored the open tabs and their order from the saved state on start;
/// ids the server no longer reports are dropped.
fn restore_tabs(
    saved: Option<&crate::persist::ServerState>,
    known: &std::collections::HashSet<String>,
) -> Vec<String> {
    let Some(saved) = saved else {
        return Vec::new();
    };
    saved
        .tabs
        .iter()
        .filter(|tab| known.contains(&tab.id))
        .map(|tab| tab.id.clone())
        .collect()
}

/// GTK's drag-to-reorder: the dragged session lands at `to`, the rows between
/// shift by one. Reports whether anything moved.
fn reorder_tabs(tabs: &mut Vec<String>, from: usize, to: usize) -> bool {
    if from == to || from >= tabs.len() || to >= tabs.len() {
        return false;
    }
    let tab = tabs.remove(from);
    tabs.insert(to, tab);
    true
}

/// Widget id of the transcript, so a `Task` can keep it at the end of the run.
fn transcript_id() -> cosmic::widget::Id {
    cosmic::widget::Id::new("opencode-transcript")
}

/// Widget id of the prompt composer, so a `Task` can put the caret in it.
fn composer_id() -> cosmic::widget::Id {
    cosmic::widget::Id::new("opencode-composer")
}

/// The GTK client's zoom ladder.
const ZOOM_STEPS: [f32; 9] = [0.7, 0.8, 0.9, 1.0, 1.1, 1.2, 1.3, 1.5, 1.75];

/// Decodes an inline `data:image/...;base64,...` URI (GTK's message images).
fn inline_image_bytes(uri: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;

    let rest = uri.strip_prefix("data:image/")?;
    let (meta, data) = rest.split_once(',')?;
    if !meta.to_lowercase().contains("base64") {
        return None;
    }
    let data = data.trim();
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(data))
        .ok()
}

/// GTK's `.session-tab-action`: dimmed to `opacity: 0.45` until the row is
/// active or hovered.
fn tab_action_class(shown: bool, radius: f32) -> cosmic::theme::Button {
    let base = move || cosmic::widget::button::Style {
        background: None,
        border_radius: radius.into(),
        border_width: 0.0,
        text_color: if shown {
            None
        } else {
            let fg = palette::current().header_title_text;
            Some(cosmic::iced::Color::from_rgba(fg.r, fg.g, fg.b, 0.45))
        },
        ..Default::default()
    };
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, _theme| base()),
        hovered: Box::new(move |_focused, _theme| base()),
        pressed: Box::new(move |_focused, _theme| base()),
        disabled: Box::new(move |_theme| base()),
    }
}

/// GTK's `button.session-tab-close:hover`: a red fill with a white glyph.
fn close_button_class(shown: bool, radius: f32) -> cosmic::theme::Button {
    let base = move || cosmic::widget::button::Style {
        background: None,
        border_radius: radius.into(),
        border_width: 0.0,
        text_color: if shown {
            None
        } else {
            let fg = palette::current().header_title_text;
            Some(cosmic::iced::Color::from_rgba(fg.r, fg.g, fg.b, 0.45))
        },
        ..Default::default()
    };
    let hovered = move || cosmic::widget::button::Style {
        background: Some(palette::current().tab_close_hover_bg.into()),
        text_color: Some(palette::current().tab_close_hover_fg),
        ..base()
    };
    let base = move || cosmic::widget::button::Style {
        text_color: if shown {
            None
        } else {
            Some(palette::current().muted_text)
        },
        ..base()
    };
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, _theme| base()),
        hovered: Box::new(move |_focused, _theme| hovered()),
        pressed: Box::new(move |_focused, _theme| hovered()),
        disabled: Box::new(move |_theme| base()),
    }
}

/// GTK's `button.queue-tray-resume`: the accent fill with a pill radius.
fn resume_button_class(radius: f32) -> cosmic::theme::Button {
    let base = move || cosmic::widget::button::Style {
        background: Some(palette::current().accent_bg.into()),
        border_radius: radius.into(),
        border_width: 0.0,
        text_color: Some(palette::current().accent_fg),
        ..Default::default()
    };
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, _theme| base()),
        hovered: Box::new(move |_focused, _theme| base()),
        pressed: Box::new(move |_focused, _theme| base()),
        disabled: Box::new(move |_theme| base()),
    }
}

/// A chip label for an attachment: the file name, middle-shortened when long.
fn attachment_label(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= 32 {
        name
    } else {
        let head: String = chars[..14].iter().collect();
        let tail: String = chars[chars.len() - 10..].iter().collect();
        format!("{head}…{tail}")
    }
}

/// `13400` -> `13.4k`, `200000` -> `200k`, `950` -> `950`.
fn compact_tokens(value: u64) -> String {
    if value < 1000 {
        return value.to_string();
    }
    let thousands = value as f64 / 1000.0;
    if (thousands.fract() * 10.0).round() < 0.5 {
        format!("{:.0}k", thousands)
    } else {
        format!("{:.1}k", thousands)
    }
}

/// The next step along [`ZOOM_STEPS`] in `direction` (clamped at both ends).
fn next_zoom(current: f32, direction: i32) -> f32 {
    if direction > 0 {
        ZOOM_STEPS
            .iter()
            .copied()
            .find(|step| *step > current + 0.04)
            .unwrap_or_else(|| *ZOOM_STEPS.last().unwrap_or(&1.0))
    } else {
        ZOOM_STEPS
            .iter()
            .rev()
            .copied()
            .find(|step| *step < current - 0.04)
            .unwrap_or_else(|| *ZOOM_STEPS.first().unwrap_or(&1.0))
    }
}

/// A 1px full-width rule, for the GTK client's `border-bottom` row separators
/// (iced's `Border` has no per-side control).
fn hairline(color: cosmic::iced::Color) -> Element<'static, Message> {
    container(row::with_children(Vec::<Element<'_, Message>>::new()))
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(move |_theme: &cosmic::Theme| container::Style {
            background: Some(color.into()),
            ..Default::default()
        })
        .into()
}

/// `HH:MM` in local time from a protocol timestamp (milliseconds, or seconds
/// when the value is small enough to be one).
fn clock_time(created: u64) -> String {
    let ms = if created > 10_000_000_000 {
        created
    } else {
        created.saturating_mul(1000)
    };
    let Ok(stamp) = jiff::Timestamp::from_millisecond(ms as i64) else {
        return String::new();
    };
    let zoned = stamp.to_zoned(jiff::tz::TimeZone::system());
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        zoned.year(),
        zoned.month(),
        zoned.day(),
        zoned.hour(),
        zoned.minute()
    )
}

/// GTK's `.composer-menu` chevron: COSMIC's dropdown draws no arrow, so the
/// menu label is followed by a small chevron icon.
fn menu_chevron(zoom: f32) -> Element<'static, Message> {
    cosmic::widget::icon::icon(icons::chevron_down())
        .size(crate::metrics::em(0.76, zoom) as u16)
        .class(cosmic::theme::Svg::custom(|_theme: &cosmic::Theme| {
            cosmic::iced::widget::svg::Style {
                color: Some(palette::current().muted_text),
            }
        }))
        .into()
}

/// GTK's suggested action (`.composer-action.suggested-action`): the client's
/// amber, not the COSMIC theme accent.
fn accent_button_class(zoom: f32) -> cosmic::theme::Button {
    let radius = crate::metrics::space(0.59, zoom);
    let base = move || cosmic::widget::button::Style {
        background: Some(palette::current().accent_bg.into()),
        border_radius: radius.into(),
        border_width: 0.0,
        text_color: Some(palette::current().accent_fg),
        ..Default::default()
    };
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, _theme| base()),
        hovered: Box::new(move |_focused, _theme| base()),
        pressed: Box::new(move |_focused, _theme| base()),
        disabled: Box::new(move |_theme| base()),
    }
}

/// GTK's `.new-session-row`: flat until hovered, then the row highlight.
fn modal_row_class(radius: f32) -> cosmic::theme::Button {
    let base = move || cosmic::widget::button::Style {
        background: None,
        border_radius: radius.into(),
        border_width: 0.0,
        text_color: Some(palette::current().header_title_text),
        ..Default::default()
    };
    let hovered = move || cosmic::widget::button::Style {
        background: Some(palette::current().sidebar_hover_bg.into()),
        ..base()
    };
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, _theme| base()),
        hovered: Box::new(move |_focused, _theme| hovered()),
        pressed: Box::new(move |_focused, _theme| base()),
        disabled: Box::new(move |_theme| base()),
    }
}

/// GTK's flat buttons (`.sidebar-nav`, `headerbar button`): no background
/// until hover. `cosmic::theme::Button::Transparent` hides the label, so the
/// class is built here with an explicit text colour.
fn flat_button_class(zoom: f32) -> cosmic::theme::Button {
    let radius = crate::metrics::space(0.5, zoom);
    let base = move || cosmic::widget::button::Style {
        background: None,
        border_radius: radius.into(),
        border_width: 0.0,
        text_color: Some(palette::current().header_title_text),
        ..Default::default()
    };
    let hovered = move || cosmic::widget::button::Style {
        background: Some(palette::current().action_hover_bg.into()),
        ..base()
    };
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, _theme| base()),
        hovered: Box::new(move |_focused, _theme| hovered()),
        pressed: Box::new(move |_focused, _theme| base()),
        disabled: Box::new(move |_theme| base()),
    }
}

/// GTK's `.queue-tray-button`: 1.85em minimum height, 1px border, its own
/// background and text colour.
fn tray_button_class(radius: f32) -> cosmic::theme::Button {
    let base = move || cosmic::widget::button::Style {
        background: Some(palette::current().tray_button_bg.into()),
        border_radius: radius.into(),
        border_width: 1.0,
        border_color: palette::current().tray_button_border,
        text_color: Some(palette::current().tray_button_text),
        ..Default::default()
    };
    let hovered = move || cosmic::widget::button::Style {
        background: Some(palette::current().action_hover_bg.into()),
        ..base()
    };
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, _theme| base()),
        hovered: Box::new(move |_focused, _theme| hovered()),
        pressed: Box::new(move |_focused, _theme| base()),
        disabled: Box::new(move |_theme| base()),
    }
}

fn tray_text_button(label: &'static str, zoom: f32, message: Message) -> Element<'static, Message> {
    let radius = crate::metrics::space(0.37, zoom);
    button::custom(text(label).size(crate::metrics::em(0.96, zoom)).class(
        cosmic::theme::Text::Color(palette::current().tray_button_text),
    ))
    .padding([
        crate::metrics::space(0.3, zoom) as u16,
        crate::metrics::space(0.7, zoom) as u16,
    ])
    .class(tray_button_class(radius))
    .on_press(message)
    .into()
}

/// The square icon twin of [`tray_text_button`].
fn tray_icon_button(
    handle: cosmic::widget::icon::Handle,
    zoom: f32,
    message: Message,
) -> Element<'static, Message> {
    let radius = crate::metrics::space(0.37, zoom);
    let size = crate::metrics::em(0.92, zoom) as u16;
    button::custom(
        cosmic::widget::icon::icon(handle)
            .size(size)
            .class(cosmic::theme::Svg::custom(|_theme: &cosmic::Theme| {
                cosmic::iced::widget::svg::Style {
                    color: Some(palette::current().tray_button_text),
                }
            })),
    )
    .padding([
        crate::metrics::space(0.3, zoom) as u16,
        crate::metrics::space(0.4, zoom) as u16,
    ])
    .class(tray_button_class(radius))
    .on_press(message)
    .into()
}

/// A bundled 16px icon painted in the theme's icon colour, for inline use.
fn inline_icon(handle: cosmic::widget::icon::Handle, zoom: f32) -> cosmic::widget::icon::Icon {
    cosmic::widget::icon::icon(handle).size(crate::metrics::em(1.23, zoom) as u16)
}

/// A small drawn status dot. The GTK client drew these with CSS; before this,
/// the port used the text glyphs `●` / `○`, which depend on the font.
/// GTK labelled every row, action and footer entry with a tooltip; libcosmic's
/// wrapper carries them again.
fn hinted<'a>(
    content: impl Into<Element<'a, Message>>,
    hint: impl Into<String>,
) -> Element<'a, Message> {
    cosmic::widget::tooltip::tooltip(
        content,
        text(hint.into()).size(13.0),
        cosmic::widget::tooltip::Position::Bottom,
    )
    .into()
}

/// GTK ellipsized a row's title at the row's width; iced clips instead, so the
/// port shortens it to roughly what fits a 272px sidebar next to its actions.
fn truncate_title(title: &str, limit: usize) -> String {
    if title.chars().count() > limit {
        let clipped: String = title.chars().take(limit - 1).collect();
        format!("{clipped}…")
    } else {
        title.to_string()
    }
}

fn status_dot(color: cosmic::iced::Color, filled: bool) -> Element<'static, Message> {
    let style = move |_theme: &cosmic::Theme| container::Style {
        background: filled.then(|| color.into()),
        border: Border {
            color,
            width: if filled { 0.0 } else { 1.0 },
            radius: 5.0.into(),
        },
        ..Default::default()
    };

    container(row::with_children(Vec::<Element<'_, Message>>::new()))
        .width(Length::Fixed(9.0))
        .height(Length::Fixed(9.0))
        .style(style)
        .into()
}

/// Maps a character to a zero-based tab index for the `1`..`9` shortcuts.
fn tab_index(c: &str) -> Option<usize> {
    let digit = c.parse::<usize>().ok()?;
    if (1..=9).contains(&digit) {
        Some(digit - 1)
    } else {
        None
    }
}

/// The first effort-menu entry is the unqualified model selection.
fn variant_selection(selection: &str) -> Option<String> {
    (!selection.is_empty()).then(|| selection.to_string())
}

impl OpenCodeCosmic {
    /// `factor` em in the current zoom, as whole pixels.
    fn em(&self, factor: f32) -> u32 {
        crate::metrics::em(factor, self.zoom)
    }

    /// `factor` em in the current zoom, as a logical pixel count for paddings.
    /// A GTK pixel value as an em factor at this zoom.
    fn pad_px(&self, px: f32) -> u16 {
        self.space(crate::metrics::px(px)) as u16
    }

    fn space(&self, factor: f32) -> f32 {
        crate::metrics::space(factor, self.zoom)
    }

    /// Opens GTK's file dialog (paperclip) on its own thread; `Tick` collects
    /// the paths, so the UI thread never blocks on the portal.
    fn pick_attachments(&mut self) {
        if self.attachment_picker.is_some() {
            return;
        }
        let (sender, receiver) = async_channel::bounded(1);
        self.attachment_picker = Some(receiver);
        std::thread::spawn(move || {
            let picked = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()
                .and_then(|runtime| {
                    runtime.block_on(
                        rfd::AsyncFileDialog::new()
                            .set_title("Attach files")
                            .pick_files(),
                    )
                })
                .map(|files| {
                    files
                        .into_iter()
                        .map(|file| file.path().to_path_buf())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let _ = sender.send_blocking(picked);
        });
    }

    /// The paths the dialog thread handed back, if it finished.
    fn take_picked_attachments(&mut self) -> Option<Vec<PathBuf>> {
        let receiver = self.attachment_picker.as_ref()?;
        match receiver.try_recv() {
            Ok(paths) => {
                self.attachment_picker = None;
                Some(paths)
            }
            Err(async_channel::TryRecvError::Empty) => None,
            Err(async_channel::TryRecvError::Closed) => {
                self.attachment_picker = None;
                None
            }
        }
    }

    /// The modal frame both palettes share.
    fn modal_frame<'a>(&'a self, title: &str, body: Element<'a, Message>) -> Element<'a, Message> {
        let radius = self.space(0.89);
        let frame = container(
            column::with_children(vec![
                row::with_children(vec![
                    text(title.to_string())
                        .size(self.em(1.14))
                        .font(cosmic::iced::Font {
                            weight: cosmic::iced::font::Weight::Semibold,
                            ..cosmic::iced::Font::DEFAULT
                        })
                        .width(Length::Fill)
                        .into(),
                    button::icon(icons::close())
                        .on_press(Message::CloseDrawer)
                        .into(),
                ])
                .align_y(Alignment::Center)
                .into(),
                body,
            ])
            .spacing(self.space(0.89))
            // GTK's sessions palette was `min-width: 39em`.
            .width(Length::Fixed(39.0 * crate::metrics::BASE_FONT_PX))
            .height(Length::Fixed(24.0 * crate::metrics::BASE_FONT_PX)),
        )
        .padding(self.space(1.04) as u16)
        .style(move |_theme: &cosmic::Theme| container::Style {
            background: Some(palette::current().modal_bg.into()),
            border: Border {
                color: palette::current().modal_border,
                width: 1.0,
                radius: radius.into(),
            },
            ..Default::default()
        });

        frame.into()
    }

    /// GTK's session picker: a search field over the session list.
    fn sessions_palette(&self) -> Element<'_, Message> {
        let mut body_items: Vec<Element<'_, Message>> = Vec::new();

        body_items.push(
            row::with_children(vec![
                inline_icon(icons::search(), self.zoom).into(),
                text_input("Search sessions...", &self.search_query)
                    .on_input(Message::SearchInput)
                    .width(Length::Fill)
                    .into(),
            ])
            .spacing(self.space(0.44))
            .align_y(Alignment::Center)
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

        let mut rows: Vec<Element<'_, Message>> = Vec::new();
        for session in filtered_sessions {
            let radius = self.space(0.44);
            rows.push(
                button::custom(
                    column::with_children(vec![
                        text(session.title.clone())
                            .size(self.em(0.96))
                            .width(Length::Fill)
                            .into(),
                        text(session.directory.clone())
                            .size(self.em(0.82))
                            .class(cosmic::theme::Text::Color(palette::current().muted_text))
                            .into(),
                    ])
                    .spacing(self.space(0.15))
                    .width(Length::Fill),
                )
                .padding([self.space(0.3) as u16, self.space(0.44) as u16])
                .width(Length::Fill)
                .class(modal_row_class(radius))
                .on_press(Message::SelectSession(session.id.clone()))
                .into(),
            );
        }

        body_items.push(
            scrollable(
                column::with_children(rows)
                    .spacing(self.space(0.15))
                    .width(Length::Fill),
            )
            .height(Length::Fill)
            .into(),
        );

        self.modal_frame(
            "Sessions",
            column::with_children(body_items)
                .spacing(self.space(0.44))
                .height(Length::Fill)
                .into(),
        )
    }

    /// GTK's new-session palette: search over the known locations.
    fn new_session_palette(&self) -> Element<'_, Message> {
        let query = self.search_query.to_lowercase();
        let mut rows: Vec<Element<'_, Message>> = Vec::new();

        for project in self.filtered_projects(&query) {
            let radius = self.space(0.44);
            let name = project
                .name
                .clone()
                .unwrap_or_else(|| project.worktree.clone());
            rows.push(
                button::custom(
                    column::with_children(vec![
                        text(name)
                            .size(self.em(0.93))
                            .font(cosmic::iced::Font {
                                weight: cosmic::iced::font::Weight::Semibold,
                                ..cosmic::iced::Font::DEFAULT
                            })
                            .width(Length::Fill)
                            .into(),
                        text(project.worktree.clone())
                            .size(self.em(0.81))
                            .class(cosmic::theme::Text::Color(palette::current().muted_text))
                            .into(),
                    ])
                    .spacing(self.space(0.15))
                    .width(Length::Fill),
                )
                .padding([self.space(0.52) as u16, self.space(0.74) as u16])
                .width(Length::Fill)
                .class(modal_row_class(radius))
                .on_press(Message::CreateSessionIn(project.worktree.clone()))
                .into(),
            );
        }

        if rows.is_empty() {
            rows.push(
                text("No locations yet — is the server reachable?")
                    .size(self.em(0.92))
                    .class(cosmic::theme::Text::Color(palette::current().muted_text))
                    .into(),
            );
        }

        let search_row: Element<'_, Message> = row::with_children(vec![
            inline_icon(icons::search(), self.zoom).into(),
            text_input("Search locations...", &self.search_query)
                .on_input(Message::SearchInput)
                .width(Length::Fill)
                .into(),
        ])
        .spacing(self.space(0.44))
        .align_y(Alignment::Center)
        .into();

        let list: Element<'_, Message> = scrollable(
            column::with_children(rows)
                .spacing(self.space(0.15))
                .width(Length::Fill),
        )
        .height(Length::Fill)
        .into();

        let body = column::with_children(vec![search_row, list])
            .spacing(self.space(0.59))
            .height(Length::Fill);

        self.modal_frame("New session", body.into())
    }

    /// GTK's rename dialog: the title entry plus the session ID with a copy
    /// button (`.session-id-field`).
    fn rename_palette(&self) -> Element<'_, Message> {
        let session_id = self.active_session_id.clone().unwrap_or_default();
        let id_radius = self.space(0.44);

        let id_field = container(
            row::with_children(vec![
                text(session_id.clone())
                    .font(cosmic::iced::Font::MONOSPACE)
                    .size(self.em(0.85))
                    .class(cosmic::theme::Text::Color(palette::current().tray_text))
                    .width(Length::Fill)
                    .into(),
                button::icon(icons::copy())
                    .padding([self.pad_px(2.0), self.pad_px(4.0)])
                    .on_press(Message::CopyText(session_id.clone()))
                    .into(),
            ])
            .spacing(self.space(0.3))
            .align_y(Alignment::Center),
        )
        .padding([self.space(0.15) as u16, self.space(0.44) as u16])
        .width(Length::Fill)
        .style(move |_theme: &cosmic::Theme| container::Style {
            background: Some(palette::current().composer_bg.into()),
            border: Border {
                color: palette::current().panel_border,
                width: 1.0,
                radius: id_radius.into(),
            },
            ..Default::default()
        });

        let body_items: Vec<Element<'_, Message>> = vec![
            text("Title")
                .size(self.em(0.82))
                .class(cosmic::theme::Text::Color(palette::current().muted_text))
                .into(),
            text_input("Session title", &self.rename_input)
                .on_input(Message::RenameInput)
                .into(),
            text("Session ID")
                .size(self.em(0.82))
                .class(cosmic::theme::Text::Color(palette::current().muted_text))
                .into(),
            id_field.into(),
            row::with_children(vec![
                button::text("Cancel").on_press(Message::CloseDrawer).into(),
                button::text("Rename")
                    .class(accent_button_class(self.zoom))
                    .on_press(Message::ApplyRename)
                    .into(),
            ])
            .spacing(self.space(0.59))
            .into(),
        ];

        let body = column::with_children(body_items)
            .spacing(self.space(0.44))
            .height(Length::Fill);

        self.modal_frame("Rename session", body.into())
    }

    fn settings_palette(&self) -> Element<'_, Message> {
        let label = |value: &'static str| -> Element<'static, Message> {
            text(value)
                .size(self.em(0.82))
                .class(cosmic::theme::Text::Color(palette::current().muted_text))
                .into()
        };

        let body = column::with_children(vec![
            label("OpenCode Server URL"),
            text_input("https://...", &self.server_url_input)
                .on_input(Message::SettingsUrlInput)
                .into(),
            label("Username"),
            text_input("opencode", &self.username_input)
                .on_input(Message::SettingsUsernameInput)
                .into(),
            label("Password"),
            text_input("Password", &self.password_input)
                .on_input(Message::SettingsPasswordInput)
                .password()
                .into(),
            button::text("Save & Connect")
                .on_press(Message::ApplySettings)
                .into(),
        ])
        .spacing(self.space(0.59))
        .height(Length::Fill);

        self.modal_frame("Connection Settings", body.into())
    }

    /// GTK's compact transcript status pill (`.transcript-status-compact`):
    /// the working or retry state below the transcript, not a card inside it.
    fn status_pill(&self, active_id: &str) -> Option<Element<'_, Message>> {
        let status = self.statuses.get(active_id);
        if !status.is_some_and(RunStatus::is_busy) {
            return None;
        }
        let retry = match status {
            Some(RunStatus::Retry { message, .. }) => Some(message.clone()),
            _ => None,
        };
        let color = if retry.is_some() {
            palette::current().status_busy
        } else {
            palette::current().status_pill_text
        };
        let label = retry.unwrap_or_else(|| "Working…".to_string());

        let pill = container(
            row::with_children(vec![
                inline_icon(icons::settings(), self.zoom)
                    .size(self.em(0.92) as u16)
                    .into(),
                text(label)
                    .size(self.em(0.96))
                    .class(cosmic::theme::Text::Color(color))
                    .into(),
                button::icon(icons::stop())
                    .on_press(Message::StopSession)
                    .into(),
            ])
            .spacing(self.space(0.59))
            .align_y(Alignment::Center),
        )
        .padding([self.space(0.52) as u16, self.space(0.89) as u16])
        .style(|_theme: &cosmic::Theme| container::Style {
            background: Some(palette::current().status_pill_bg.into()),
            border: Border {
                color: palette::current().status_pill_border,
                width: 1.0,
                radius: 999.0.into(),
            },
            ..Default::default()
        });

        Some(
            container(pill)
                .padding([
                    self.space(0.59) as u16,
                    self.space(2.07) as u16,
                    self.space(0.74) as u16,
                    self.space(2.07) as u16,
                ])
                .into(),
        )
    }

    /// Moves one step along [`ZOOM_STEPS`] and persists the result.
    fn zoom_step(&mut self, direction: i32) {
        self.set_zoom(next_zoom(self.zoom, direction));
    }

    fn set_zoom(&mut self, zoom: f32) {
        if (zoom - self.zoom).abs() < 0.001 {
            return;
        }
        self.zoom = zoom;
        self.state.zoom_level = f64::from(zoom);
        let _ = self.state.save(&default_path());
    }

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
                    directories: self
                        .projects
                        .iter()
                        .map(|project| project.worktree.clone())
                        .collect(),
                });
            }
            Err(e) => {
                self.error_banner = Some(format!("Failed to connect: {e}"));
                self.connection_status = "Connection Failed".to_string();
            }
        }
    }

    fn drain_events(&mut self) {
        // Any answer settles the previous tray request; GTK re-enabled Resume
        // when the row's request came back.
        self.tray_in_flight = false;
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

                self.projects = bootstrap.projects.clone();
                self.replace_pending(&bootstrap.pending);

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

                let saved = self.state.servers.get(&self.state.connection.server);
                if self.tabs.is_empty() {
                    let known: std::collections::HashSet<String> =
                        self.sessions.keys().cloned().collect();
                    self.tabs = restore_tabs(saved, &known);
                    if let Some(unread) = saved.map(|state| state.unread.clone()) {
                        self.unread = unread;
                        self.unread.retain(|id| self.tabs.contains(id));
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
                    // GTK reopened the tab that was active, when it still exists.
                    self.active_session_id = saved
                        .and_then(|state| state.active.clone())
                        .filter(|id| self.tabs.contains(id))
                        .or_else(|| self.tabs.first().cloned());
                    self.focus_composer = self.active_session_id.is_some();
                }
                self.persist_tabs();

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
                self.history_loading = false;
                let conv = self.conversations.entry(session_id).or_default();
                if cursor.is_none() {
                    conv.replace_from_api(&page.messages, page.next_cursor);
                    // The page carries the session's inbox: without it a
                    // parked session (queued before this client looked) shows
                    // an empty tray until some live event happens.
                    if let Some(inbox) = &page.queued {
                        conv.sync_queued(inbox);
                    }
                } else {
                    conv.prepend_from_api(&page.messages, page.next_cursor);
                }
            }
            UiEvent::PendingLoaded(snapshot) => {
                self.replace_pending(&snapshot.requests);
                if let Some(warning) = snapshot.warnings.first() {
                    self.error_banner = Some(warning.clone());
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
                    // GTK marked a tab unread when its run went idle while the
                    // user was looking elsewhere.
                    let ran = self
                        .statuses
                        .get(&sid)
                        .is_some_and(|previous| previous.is_busy());
                    if ran
                        && !status.is_busy()
                        && self.active_session_id.as_deref() != Some(sid.as_str())
                        && self.tabs.iter().any(|tab| tab.as_str() == sid)
                    {
                        self.unread.insert(sid.to_string());
                    }
                    self.statuses.insert(sid.to_string(), status);
                }

                if let Ok(event) = protocol::Event::deserialize(&envelope.payload) {
                    let kind = protocol::decode_event(&event);

                    if let Some(sid) = kind.session_id() {
                        let conv = self.conversations.entry(sid.to_string()).or_default();
                        conv.apply(&event, &kind);
                    }

                    match crate::pending::pending_change(&kind, envelope.directory.as_deref()) {
                        Some(crate::pending::PendingChange::Permission { directory, request }) => {
                            self.absorb_pending(crate::pending::PendingRequest::Permission {
                                directory: directory.unwrap_or_default(),
                                request,
                            });
                        }
                        Some(crate::pending::PendingChange::Form(form)) => {
                            self.absorb_pending(crate::pending::PendingRequest::Form(form));
                        }
                        Some(crate::pending::PendingChange::Resolved(id)) => {
                            self.resolve_pending(&id)
                        }
                        None => {}
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
                self.focus_composer = true;

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

    /// GTK saved the open tabs, their order, the active one and the unread
    /// markers per server, so the next start reopens the same sessions.
    fn persist_tabs(&mut self) {
        // The preview drives a mock server: never write its tabs into the real
        // state file.
        if self.mock_server.is_some() {
            return;
        }
        let key = self.state.connection.server.clone();
        if key.is_empty() {
            return;
        }
        let tabs: Vec<crate::persist::PersistedTab> = self
            .tabs
            .iter()
            .map(|id| {
                let session = self.sessions.get(id);
                crate::persist::PersistedTab {
                    id: id.clone(),
                    directory: session
                        .map(|session| session.directory.clone())
                        .unwrap_or_default(),
                    title: session
                        .map(|session| session.title.clone())
                        .unwrap_or_default(),
                }
            })
            .collect();
        let unread: std::collections::HashSet<String> = self.unread.clone();
        let active = self.active_session_id.clone();
        let entry = self.state.servers.entry(key).or_default();
        entry.tabs = tabs;
        entry.active = active;
        entry.unread = unread;
        let _ = self.state.save(&default_path());
    }

    fn set_active_session(&mut self, id: &str) {
        self.active_session_id = Some(id.to_string());
        self.focus_composer = true;
        self.unread.remove(id);
        self.persist_tabs();
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
        self.persist_tabs();
    }

    /// Moves the active tab by `delta` positions, wrapping around.
    fn cycle_tab(&mut self, delta: i32) {
        if self.tabs.len() < 2 {
            return;
        }
        let current = self
            .active_session_id
            .as_ref()
            .and_then(|id| self.tabs.iter().position(|tab| tab == id))
            .unwrap_or(0);
        let len = self.tabs.len() as i32;
        let next = (current as i32 + delta).rem_euclid(len) as usize;
        let id = self.tabs[next].clone();
        self.set_active_session(&id);
    }

    fn open_session(&mut self, id: &str) {
        if !self.tabs.contains(&id.to_string()) {
            self.tabs.push(id.to_string());
        }
        self.set_active_session(id);
    }

    /// GTK's new-session palette: create in the first location that matches
    /// the palette's search, so `Ctrl+T` then `Enter` still makes a session.
    fn confirm_new_session(&mut self) {
        let query = self.search_query.to_lowercase();
        let directory = self
            .filtered_projects(&query)
            .first()
            .map(|project| project.worktree.clone());
        if let Some(directory) = directory {
            self.search_query.clear();
            self.active_drawer = None;
            self.create_session(&directory);
        }
    }

    /// The locations matching a search over name and path, in list order.
    fn filtered_projects(&self, query: &str) -> Vec<&model::Project> {
        self.projects
            .iter()
            .filter(|project| {
                query.is_empty()
                    || project.worktree.to_lowercase().contains(query)
                    || project
                        .name
                        .as_deref()
                        .is_some_and(|name| name.to_lowercase().contains(query))
            })
            .collect()
    }

    fn active_session_title(&self) -> String {
        self.active_session_id
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .map(|session| session.title.clone())
            .unwrap_or_default()
    }

    fn apply_rename(&mut self) {
        let Some(session_id) = self
            .rename_target
            .clone()
            .or_else(|| self.active_session_id.clone())
        else {
            return;
        };
        let title = self.rename_input.trim().to_string();
        if title.is_empty() {
            return;
        }
        let req_id = self.next_request_id();
        if let Some(api) = &self.api {
            api.send(Command::RenameSession {
                request_id: req_id,
                session_id,
                title,
            });
        }
        self.active_drawer = None;
        self.rename_target = None;
    }

    fn create_session(&mut self, directory: &str) {
        let req_id = self.next_request_id();
        if let Some(api) = &self.api {
            api.send(Command::CreateSession {
                request_id: req_id,
                directory: directory.to_string(),
                title: None,
            });
        } else if let Some(mock) = &mut self.mock_server {
            let event = mock.handle(Command::CreateSession {
                request_id: req_id,
                directory: directory.to_string(),
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
        let attachments = std::mem::take(&mut self.pending_attachments);
        let req_id = self.next_request_id();
        let msg_id = format!("msg_{}", req_id);

        if let Some(api) = &self.api {
            api.send(Command::SendPrompt {
                request_id: req_id,
                message_id: msg_id,
                session_id: active_id,
                text: prompt,
                attachments: attachments.clone(),
                delivery: mode.delivery(),
            });
        } else if let Some(mock) = &mut self.mock_server {
            let event = mock.handle(Command::SendPrompt {
                request_id: req_id,
                message_id: msg_id,
                session_id: active_id,
                text: prompt,
                attachments,
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

    /// Loads the next older page when the transcript reaches its top.
    fn load_older_history(&mut self) {
        // A little slack so the next page is there when the user arrives,
        // instead of after a visible pause.
        if self.transcript_from_top > 24.0 || self.history_loading {
            return;
        }
        let Some(active_id) = self.active_session_id.clone() else {
            return;
        };
        let Some(cursor) = self
            .conversations
            .get(&active_id)
            .and_then(|conversation| conversation.next_cursor.clone())
        else {
            return;
        };
        if let Some(api) = &self.api {
            api.send(Command::LoadMessages {
                session_id: active_id,
                cursor: Some(cursor),
            });
            self.history_loading = true;
        } else if let Some(mock) = &mut self.mock_server {
            let event = mock.handle(Command::LoadMessages {
                session_id: active_id,
                cursor: Some(cursor),
            });
            self.handle_ui_event(event);
        }
    }

    /// The current turn's request, for GTK's sticky prompt: pinned once its own
    /// row has left the top of the transcript (the answer started), or while
    /// the reader has scrolled back up through a long one.
    fn sticky_prompt(&self) -> Option<(String, String)> {
        let conversation = self
            .active_session_id
            .as_ref()
            .and_then(|id| self.conversations.get(id))?;
        let index = conversation
            .messages
            .iter()
            .rposition(|message| message.role == model::Role::User && !message.in_tray())?;
        let has_answer = conversation
            .messages
            .iter()
            .skip(index + 1)
            .any(|message| message.role == model::Role::Assistant);
        if !(has_answer && self.transcript_from_top > 8.0) && self.transcript_from_bottom <= 40.0 {
            return None;
        }
        let message = &conversation.messages[index];
        let text = message
            .segments()
            .iter()
            .filter(|segment| segment.kind == model::SegmentKind::Text)
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        (!text.trim().is_empty()).then(|| (message.id.clone(), text))
    }

    /// Adds a pending request, replacing a known one of the same id.
    fn absorb_pending(&mut self, request: crate::pending::PendingRequest) {
        let id = request.id().to_string();
        match request {
            crate::pending::PendingRequest::Permission { .. } => {
                if self.permissions.iter().all(|known| known.id() != id) {
                    self.permissions.push(request);
                }
            }
            crate::pending::PendingRequest::Form(form) => self.forms.upsert(form),
        }
    }

    /// Drops a request the server settled (a reply, an answer, a cancel).
    fn resolve_pending(&mut self, id: &str) {
        self.permissions.retain(|known| known.id() != id);
        self.forms.remove(id);
    }

    /// Replaces the whole set (bootstrap and reconciliations).
    fn replace_pending(&mut self, requests: &[crate::pending::PendingRequest]) {
        self.permissions.clear();
        self.forms.clear();
        for request in requests {
            self.absorb_pending(request.clone());
        }
    }

    /// Session -> parent, for the form visibility rules.
    fn session_parents(&self) -> HashMap<String, String> {
        self.sessions
            .iter()
            .filter_map(|(id, session)| {
                session
                    .parent_id
                    .as_ref()
                    .map(|parent| (id.clone(), parent.clone()))
            })
            .collect()
    }

    /// GTK's form notice (`.form-notice`), which `Ctrl+Shift+X` cancels.
    fn form_notice(&self) -> Option<crate::pending::FormNotice> {
        self.forms
            .notice(self.active_session_id.as_deref(), &self.session_parents())
    }

    /// Permission prompts whose scope is the active session: its own, or a
    /// child's whose parent is one of the root sessions (`pending`'s rule), so
    /// a prompt that blocks a running subagent shows where it belongs.
    fn visible_permissions(&self) -> Vec<&protocol::PermissionRequest> {
        let Some(active) = self.active_session_id.as_deref() else {
            return Vec::new();
        };
        let parents = self.session_parents();
        let is_root = |id: &str| {
            self.sessions
                .get(id)
                .is_none_or(|session| session.parent_id.is_none())
        };
        self.permissions
            .iter()
            .filter_map(|request| match request {
                crate::pending::PendingRequest::Permission { request, .. } => Some(request),
                crate::pending::PendingRequest::Form(_) => None,
            })
            .filter(|request| {
                crate::pending::permission_scope(&request.session_id, is_root, &parents)
                    .is_none_or(|scope| scope == active)
            })
            .collect()
    }

    /// The active session's waiting prompts as the tray engine's rows.
    fn tray_rows(&self) -> Vec<crate::tray::TrayRow> {
        self.active_session_id
            .as_ref()
            .and_then(|id| self.conversations.get(id))
            .map(|conversation| {
                conversation
                    .tray_items()
                    .into_iter()
                    .map(|item| crate::tray::TrayRow {
                        id: item.id,
                        delivery: item.delivery,
                        summary: item.text,
                        sending: false,
                        in_flight: false,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// GTK's Resume: wakes a parked session for every waiting message.
    fn resume_tray(&mut self) {
        if self.tray_in_flight {
            return;
        }
        let Some(active_id) = self.active_session_id.clone() else {
            return;
        };
        let Some((inbox_id, request)) = crate::tray::resume_request(&self.tray_rows()) else {
            return;
        };
        if let Some(api) = &self.api {
            api.send(Command::Inbox {
                session_id: active_id,
                inbox_id,
                request,
            });
            self.tray_in_flight = true;
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
        self.send_model_selection(model_id, None);
    }

    fn switch_variant(&mut self, variant: &str) {
        let Some(model_id) = self.active_session_model_id() else {
            return;
        };
        self.send_model_selection(&model_id, variant_selection(variant));
    }

    fn send_model_selection(&mut self, model_id: &str, variant: Option<String>) {
        let Some(active_id) = self.active_session_id.clone() else {
            return;
        };

        // The provider ID comes from the catalog; only fall back to the
        // historical default when the model is not in it.
        let directory = self
            .sessions
            .get(&active_id)
            .map(|s| s.directory.clone())
            .unwrap_or_default();
        let provider_id = self
            .catalogs
            .get(&directory)
            .and_then(|catalog| {
                catalog
                    .models
                    .iter()
                    .find(|m| m.model_id == model_id)
                    .map(|m| m.provider_id.clone())
            })
            .unwrap_or_else(|| "anthropic".to_string());

        let req_id = self.next_request_id();
        if let Some(api) = &self.api {
            api.send(Command::SelectModel {
                request_id: req_id,
                session_id: active_id,
                model: protocol::ModelRef {
                    id: model_id.to_string(),
                    provider_id,
                    variant,
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

    fn active_session_model_id(&self) -> Option<String> {
        self.active_session_id
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .and_then(|s| s.model.as_ref())
            .map(|m| m.id.clone())
    }

    /// GTK's session-header strip shows the raw count (`13400 tokens`).
    fn context_usage_raw(&self) -> String {
        self.active_session_id
            .as_ref()
            .and_then(|id| self.conversations.get(id))
            .and_then(|conversation| conversation.context_tokens())
            .map(|tokens| format!("{tokens} tokens"))
            .unwrap_or_default()
    }

    /// GTK's `.composer-usage`: compact tokens against the model's window,
    /// e.g. `13.4k / 200k`.
    fn active_context_usage(&self) -> String {
        let Some(tokens) = self
            .active_session_id
            .as_ref()
            .and_then(|id| self.conversations.get(id))
            .and_then(|c| c.context_tokens())
        else {
            return String::new();
        };
        let limit = self
            .active_session_id
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .and_then(|s| s.model.as_ref())
            .map(|m| m.id.clone())
            .and_then(|id| {
                let directory = self
                    .active_session_id
                    .as_ref()
                    .and_then(|sid| self.sessions.get(sid))
                    .map(|s| s.directory.clone())?;
                self.catalogs
                    .get(&directory)
                    .and_then(|catalog| catalog.models.iter().find(|m| m.model_id == id).cloned())
                    .and_then(|model| model.context_limit)
            });
        match limit {
            Some(limit) => format!("{} / {}", compact_tokens(tokens), compact_tokens(limit)),
            None => compact_tokens(tokens),
        }
    }

    fn legacy_active_context_usage(&self) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_default_and_named_selection_map_to_model_variants() {
        assert_eq!(variant_selection(""), None);
        assert_eq!(variant_selection("medium"), Some("medium".to_string()));
    }

    fn ch(value: &str) -> Key {
        Key::Character(value.into())
    }

    fn message(key: &Key, modifiers: Modifiers) -> Option<Message> {
        shortcut(key, modifiers)
    }

    #[test]
    fn advertised_shortcuts_resolve() {
        assert!(matches!(
            message(&ch("p"), Modifiers::CTRL),
            Some(Message::ToggleDrawer(DrawerPage::Sessions))
        ));
        assert!(matches!(
            message(&ch(","), Modifiers::CTRL),
            Some(Message::ToggleDrawer(DrawerPage::Settings))
        ));
        assert!(matches!(
            message(&ch("t"), Modifiers::CTRL),
            Some(Message::NewSession)
        ));
        assert!(matches!(
            message(&ch("w"), Modifiers::CTRL),
            Some(Message::CloseActiveTab)
        ));
        assert!(matches!(
            message(&ch("b"), Modifiers::CTRL),
            Some(Message::ToggleSidebar)
        ));
        assert!(matches!(
            message(&ch("g"), Modifiers::CTRL),
            Some(Message::FocusComposer)
        ));
    }

    #[test]
    fn uppercase_variants_match_too() {
        assert!(matches!(
            message(&ch("P"), Modifiers::CTRL),
            Some(Message::ToggleDrawer(DrawerPage::Sessions))
        ));
        assert!(matches!(
            message(&ch("T"), Modifiers::CTRL),
            Some(Message::NewSession)
        ));
    }

    #[test]
    fn enter_reports_whether_ctrl_is_held() {
        let enter = Key::Named(Named::Enter);
        assert!(matches!(
            message(&enter, Modifiers::NONE),
            Some(Message::ComposerEnter { ctrl: false })
        ));
        assert!(matches!(
            message(&enter, Modifiers::CTRL),
            Some(Message::ComposerEnter { ctrl: true })
        ));
        // The run status turns these into send/steer/queue (tray::enter_mode).
        assert_eq!(enter_mode(false, false), SendMode::Send);
        assert_eq!(enter_mode(true, false), SendMode::Steer);
        assert_eq!(enter_mode(true, true), SendMode::Queue);
    }

    #[test]
    fn tab_cycling_wraps_with_shift() {
        assert!(matches!(
            message(&Key::Named(Named::Tab), Modifiers::CTRL),
            Some(Message::CycleTab(1))
        ));
        assert!(matches!(
            message(&Key::Named(Named::Tab), Modifiers::CTRL | Modifiers::SHIFT),
            Some(Message::CycleTab(-1))
        ));
    }

    #[test]
    fn digits_select_tabs_with_ctrl_or_alt() {
        assert!(matches!(
            message(&ch("3"), Modifiers::CTRL),
            Some(Message::SelectTabIndex(2))
        ));
        assert!(matches!(
            message(&ch("9"), Modifiers::ALT),
            Some(Message::SelectTabIndex(8))
        ));
        // Ctrl+0 resets the zoom (GTK's Ctrl+= / Ctrl+- / Ctrl+0 ladder).
        assert!(matches!(
            message(&ch("0"), Modifiers::CTRL),
            Some(Message::ZoomReset)
        ));
    }

    #[test]
    fn zoom_keys_follow_the_gtk_ladder() {
        for key in ["=", "+"] {
            assert!(matches!(
                message(&ch(key), Modifiers::CTRL),
                Some(Message::ZoomIn)
            ));
        }
        assert!(matches!(
            message(&ch("-"), Modifiers::CTRL),
            Some(Message::ZoomOut)
        ));
        assert!(matches!(
            message(&ch("0"), Modifiers::CTRL),
            Some(Message::ZoomReset)
        ));
        assert_eq!(ZOOM_STEPS.first(), Some(&0.7));
        assert_eq!(ZOOM_STEPS.last(), Some(&1.75));
    }

    #[test]
    fn zoom_ladder_matches_the_gtk_steps() {
        assert_eq!(next_zoom(1.0, 1), 1.1);
        assert_eq!(next_zoom(1.0, -1), 0.9);
        assert_eq!(next_zoom(1.3, 1), 1.5);
        assert_eq!(next_zoom(0.7, -1), 0.7, "clamped at the bottom");
        assert_eq!(next_zoom(1.75, 1), 1.75, "clamped at the top");
        // 1em and the GTK spacing scale grow with the zoom.
        assert_eq!(crate::metrics::em(1.0, 1.0), 13);
        assert_eq!(crate::metrics::em(0.76, 1.75), 17);
        assert!((crate::metrics::space(1.19, 1.2) - 18.564).abs() < 0.01);
    }

    #[test]
    fn inline_images_decode_only_base64_image_uris() {
        assert_eq!(
            inline_image_bytes("data:image/png;base64,aGk="),
            Some(b"hi".to_vec())
        );
        assert_eq!(inline_image_bytes("data:text/plain;base64,aGk="), None);
        assert_eq!(inline_image_bytes("data:image/png;base64,@@ nope @@"), None);
        assert_eq!(inline_image_bytes("https://example.com/x.png"), None);
    }

    #[test]
    fn attachment_labels_use_the_file_name_and_shorten_middle() {
        assert_eq!(
            attachment_label(std::path::Path::new("/state/home/paperclip-22px.png")),
            "paperclip-22px.png"
        );
        let long = attachment_label(std::path::Path::new(
            "/tmp/a-very-long-name-for-a-screenshot-of-the-composer.png",
        ));
        assert!(long.contains('…'), "{long}");
        assert!(long.chars().count() <= 26, "{long}");
    }

    #[test]
    fn compact_tokens_matches_the_gtk_usage_line() {
        assert_eq!(compact_tokens(950), "950");
        assert_eq!(compact_tokens(13_400), "13.4k");
        assert_eq!(compact_tokens(200_000), "200k");
        assert_eq!(compact_tokens(1_000), "1k");
        assert_eq!(compact_tokens(1_050), "1.1k");
    }

    #[test]
    fn clock_formats_milliseconds_and_seconds() {
        // GTK printed `YYYY-MM-DD HH:MM`; both protocol shapes must render.
        for value in [1_790_000_000_000u64, 1_790_000_000] {
            let stamp = clock_time(value);
            assert_eq!(stamp.len(), 16, "{stamp}");
            assert_eq!(&stamp[4..5], "-");
            assert_eq!(&stamp[10..11], " ");
            assert_eq!(&stamp[13..14], ":");
        }
    }

    #[test]
    fn escape_closes_the_drawer() {
        assert!(matches!(
            message(&Key::Named(Named::Escape), Modifiers::NONE),
            Some(Message::CloseDrawer)
        ));
    }

    #[test]
    fn typed_text_is_never_swallowed() {
        // Plain keys must reach the composer instead of triggering a shortcut.
        for value in ["t", "p", "b", "w", ",", "1", "9", "0"] {
            assert!(
                message(&ch(value), Modifiers::NONE).is_none(),
                "plain {value:?} must not trigger a shortcut"
            );
            assert!(
                message(&ch(value), Modifiers::SHIFT).is_none(),
                "shift+{value:?} must not trigger a shortcut"
            );
        }
        assert!(message(&ch("q"), Modifiers::CTRL).is_none());
    }

    #[test]
    fn saved_tabs_are_restored_without_sessions_the_server_lost() {
        let known: std::collections::HashSet<String> =
            ["a".to_string(), "c".to_string()].into_iter().collect();
        let saved = crate::persist::ServerState {
            tabs: ["c", "gone", "a"]
                .iter()
                .map(|id| crate::persist::PersistedTab {
                    id: (*id).to_string(),
                    directory: "/repo".to_string(),
                    title: (*id).to_string(),
                })
                .collect(),
            ..Default::default()
        };

        assert_eq!(restore_tabs(Some(&saved), &known), vec!["c", "a"]);
        assert!(restore_tabs(None, &known).is_empty());
    }

    #[test]
    fn dragging_a_session_moves_it_and_shifts_the_rest() {
        let tabs = |ids: &[&str]| ids.iter().map(|id| (*id).to_string()).collect::<Vec<_>>();

        let mut open = tabs(&["a", "b", "c", "d", "e"]);
        assert!(reorder_tabs(&mut open, 4, 1));
        assert_eq!(open, tabs(&["a", "e", "b", "c", "d"]));

        let mut open = tabs(&["a", "b", "c"]);
        assert!(reorder_tabs(&mut open, 0, 2));
        assert_eq!(open, tabs(&["b", "c", "a"]));

        // Dropping a row on itself, or out of range, leaves the order alone.
        let mut open = tabs(&["a", "b"]);
        assert!(!reorder_tabs(&mut open, 1, 1));
        assert!(!reorder_tabs(&mut open, 0, 5));
        assert_eq!(open, tabs(&["a", "b"]));
    }
}
