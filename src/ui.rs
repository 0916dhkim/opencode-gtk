use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::PathBuf,
    rc::{Rc, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_channel::Receiver;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use gtk::{gdk, gio, glib, pango, prelude::*};
use serde::Deserialize;

use crate::{
    api::{ApiConfig, ApiHandle, Bootstrap, Command, MessagePage, ServerEnvelope, UiEvent},
    credentials::{self, CloudflareAccessCredentials},
    markdown,
    model::{
        deleted_session_id, event_data, event_run_status, event_session, event_session_id,
        format_context_usage, Conversation, ModelCatalog, ModelOption, ModelSelection, Project,
        RunStatus, Session, SessionTime,
    },
    persist::{default_path, ConnectionSettings, PersistedState, PersistedTab, ServerState},
};

const STREAM_FRAME: Duration = Duration::from_millis(33);
const BOOTSTRAP_RETRY_MIN: Duration = Duration::from_secs(2);
const BOOTSTRAP_RETRY_MAX: Duration = Duration::from_secs(30);
const SESSION_PICKER_LIMIT: usize = 200;
const ICON_SEND: &str = "opencode-send-symbolic";
const ICON_STOP: &str = "opencode-stop-symbolic";
const ICON_EDIT: &str = "opencode-edit-symbolic";
const ICON_CLOSE: &str = "opencode-close-symbolic";
const ICON_ADD: &str = "opencode-add-symbolic";
const ICON_SESSIONS: &str = "opencode-sessions-symbolic";
const ICON_SETTINGS: &str = "opencode-settings-symbolic";
const ICON_CONNECTION: &str = "opencode-connection-symbolic";
const ICON_SEARCH: &str = "opencode-search-symbolic";
const COMPOSER_ICON_PX: i32 = 22;
const TAB_ICON_PX: i32 = 16;
const BOTTOM_EPSILON: f64 = 2.0;
const MAX_INLINE_IMAGE_BYTES: usize = 25 * 1024 * 1024;
const ROW_ESTIMATE: i32 = 88;
const ROW_MIN_HEIGHT: i32 = 24;
const ROW_OVERSCAN: usize = 4;
const ROW_PADDING_X: i32 = 56;

#[derive(Deserialize)]
struct TranscriptRow {
    role: String,
    body: String,
    #[serde(default)]
    images: Vec<String>,
    #[serde(default)]
    time: u64,
    #[serde(default)]
    kind: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TranscriptUpdate {
    Activate,
    Content,
    Prepend,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TranscriptIndicator {
    Hidden,
    NoSession,
    Loading,
    Refreshing,
    Working,
    Retry(String),
    Error,
    Empty,
}

struct TranscriptVisible {
    index: usize,
    bound: String,
    row: gtk::Box,
}

#[derive(Clone, Debug, Default)]
struct Draft {
    text: String,
    attachments: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
struct PendingPrompt {
    request_id: u64,
    draft: Draft,
}

#[derive(Clone, Debug)]
struct OptimisticPrompt {
    row: String,
    baseline_you: usize,
}

#[derive(Clone)]
struct QuestionInputs {
    options: Vec<(gtk::CheckButton, String)>,
    custom: Option<gtk::Entry>,
    multiple: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AppModalKind {
    Sessions,
    Settings,
    Rename,
}

#[derive(Clone)]
struct ComposerPrompt {
    request_id: String,
    session_id: Option<String>,
    widget: gtk::Widget,
}

#[derive(Default)]
struct State {
    sessions: Vec<Session>,
    projects: Vec<Project>,
    tabs: Vec<String>,
    active: Option<String>,
    conversations: HashMap<String, Conversation>,
    catalogs: HashMap<String, ModelCatalog>,
    selections: HashMap<String, ModelSelection>,
    statuses: HashMap<String, RunStatus>,
    unread: HashSet<String>,
    drafts: HashMap<String, Draft>,
    pending_prompts: HashMap<String, PendingPrompt>,
    optimistic_prompts: HashMap<String, OptimisticPrompt>,
    server_busy: HashSet<String>,
    abort_requested: HashSet<String>,
    loading_messages: HashSet<String>,
    loading_models: HashSet<String>,
}

#[derive(Clone)]
struct Widgets {
    window: gtk::ApplicationWindow,
    sidebar: gtk::Box,
    sidebar_toggle: gtk::Button,
    session_button: gtk::Button,
    new_button: gtk::Button,
    settings_button: gtk::Button,
    status: gtk::Label,
    session_header_bar: gtk::Box,
    session_header_status: gtk::Box,
    session_header_title: gtk::Label,
    tab_bar: gtk::Box,
    transcript: gtk::Box,
    transcript_spacer: gtk::Box,
    transcript_tail: gtk::Box,
    transcript_scroll: gtk::ScrolledWindow,
    sticky_message: gtk::Box,
    sticky_message_scroll: gtk::ScrolledWindow,
    sticky_message_body: gtk::Label,
    sticky_message_time: gtk::Label,
    transcript_status: gtk::Box,
    transcript_spinner: gtk::Spinner,
    transcript_status_label: gtk::Label,
    load_earlier: gtk::Button,
    composer_stack: gtk::Stack,
    prompt_host: gtk::Box,
    composer: gtk::TextView,
    attachment_box: gtk::Box,
    attach_button: gtk::Button,
    model_button: gtk::Button,
    model_button_label: gtk::Label,
    model_popover: gtk::Popover,
    model_search: gtk::SearchEntry,
    model_list: gtk::ListBox,
    model_filtered_indices: Rc<RefCell<Vec<usize>>>,
    variant_button: gtk::Button,
    variant_button_label: gtk::Label,
    variant_popover: gtk::Popover,
    variant_search: gtk::SearchEntry,
    variant_list: gtk::ListBox,
    variant_filtered_indices: Rc<RefCell<Vec<usize>>>,
    new_session_overlay: gtk::Box,
    new_session_card: gtk::Box,
    app_modal_overlay: gtk::Box,
    app_modal_card: gtk::Box,
    new_session_search: gtk::Entry,
    new_session_list: gtk::ListBox,
    new_session_filtered_paths: Rc<RefCell<Vec<String>>>,
    context_usage: gtk::Label,
    send_button: gtk::Button,
    transcript_user_scrolling: Rc<Cell<bool>>,
    tab_dnd: Rc<RefCell<TabDnd>>,
}

#[derive(Default)]
struct TabDnd {
    index: Option<usize>,
    slot: Option<gtk::Revealer>,
    gen: u64,
}

struct Controller {
    api: ApiHandle,
    events: Receiver<UiEvent>,
    connection_config: ApiConfig,
    connection_generation: u64,
    server_key: String,
    state_path: PathBuf,
    persisted: PersistedState,
    persistence_warning: Option<String>,
    persistence_error: Option<String>,
    credential_warning: Option<String>,
    persistence_writes_blocked: bool,
    had_server_state: bool,
    offline_busy: HashSet<String>,
    state: State,
    widgets: Widgets,
    rendered_session: Option<String>,
    rendered_rows: Vec<String>,
    transcript_heights: Vec<i32>,
    transcript_visible: Vec<TranscriptVisible>,
    transcript_pool: Vec<TranscriptVisible>,
    transcript_at_bottom: bool,
    transcript_scroll_value: f64,
    transcript_scroll_generation: u64,
    transcript_edge_refresh_scheduled: bool,
    transcript_edge_remeasure: bool,
    transcript_layout_width: i32,
    current_models: Vec<ModelOption>,
    current_variants: Vec<Option<String>>,
    controls_updating: bool,
    pending_events: Vec<ServerEnvelope>,
    event_flush_scheduled: bool,
    session_events_during_bootstrap: HashMap<String, Option<Session>>,
    session_removals_during_bootstrap: HashMap<String, u64>,
    status_events_during_bootstrap: HashMap<String, RunStatus>,
    resolved_requests_during_bootstrap: HashSet<String>,
    message_events_during_load: HashMap<String, Vec<serde_json::Value>>,
    replacing_messages: HashSet<String>,
    message_reload_pending: HashSet<String>,
    message_load_errors: HashMap<String, String>,
    models_reload_pending: HashSet<String>,
    model_load_errors: HashMap<String, String>,
    bootstrap_pending: bool,
    bootstrap_reload_pending: bool,
    bootstrap_dialogs_at_start: HashSet<String>,
    bootstrap_retry_token: u64,
    bootstrap_retry_delay: Duration,
    self_weak: Weak<RefCell<Controller>>,
    dialogs: HashMap<String, gtk::Widget>,
    pending_actions: HashSet<String>,
    composer_prompts: Vec<ComposerPrompt>,
    shown_composer_prompt: Option<String>,
    app_modal: Option<AppModalKind>,
    app_modal_focus: Option<gtk::Widget>,
    session_picker: Option<(gtk::ListBox, gtk::Entry)>,
    rename_session_id: Option<String>,
    next_session_request_id: u64,
    pending_session_request: Option<u64>,
    pending_rename_request: Option<u64>,
    next_prompt_request_id: u64,
    connected_once: bool,
    event_connected: bool,
    tab_shortcut_hint: bool,
}

fn register_icons() {
    gio::resources_register_include!("icons.gresource").expect("register icon resources");
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_resource_path("/ai/opencode/gtk/icons");
    }
}

fn icon_image(name: &str, pixel_size: i32) -> gtk::Image {
    let image = gtk::Image::from_icon_name(name);
    image.set_pixel_size(pixel_size);
    image
}

fn icon_button(name: &str, pixel_size: i32) -> gtk::Button {
    let button = gtk::Button::new();
    button.set_child(Some(&icon_image(name, pixel_size)));
    button
}

fn copy_text_button(text: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.set_child(Some(&markdown::copy_icon(14)));
    button.set_tooltip_text(Some(tooltip));
    button.set_valign(gtk::Align::Center);
    button.add_css_class("flat");
    button.add_css_class("session-id-copy");
    let copy_text = text.to_owned();
    let idle_tooltip = tooltip.to_owned();
    let generation = Rc::new(Cell::new(0u32));
    button.connect_clicked(move |btn| {
        btn.display().clipboard().set_text(&copy_text);
        btn.set_child(Some(&markdown::check_icon(14)));
        btn.add_css_class("copied");
        btn.set_tooltip_text(Some("Copied"));
        let next = generation.get().wrapping_add(1);
        generation.set(next);
        let btn = btn.clone();
        let generation = generation.clone();
        let idle_tooltip = idle_tooltip.clone();
        glib::timeout_add_local_once(Duration::from_millis(1200), move || {
            if generation.get() != next || !btn.is_visible() {
                return;
            }
            btn.set_child(Some(&markdown::copy_icon(14)));
            btn.remove_css_class("copied");
            btn.set_tooltip_text(Some(&idle_tooltip));
        });
    });
    button
}

fn tab_index_from_key(key: gdk::Key) -> Option<usize> {
    match key {
        gdk::Key::_1 | gdk::Key::KP_1 => Some(0),
        gdk::Key::_2 | gdk::Key::KP_2 => Some(1),
        gdk::Key::_3 | gdk::Key::KP_3 => Some(2),
        gdk::Key::_4 | gdk::Key::KP_4 => Some(3),
        gdk::Key::_5 | gdk::Key::KP_5 => Some(4),
        gdk::Key::_6 | gdk::Key::KP_6 => Some(5),
        gdk::Key::_7 | gdk::Key::KP_7 => Some(6),
        gdk::Key::_8 | gdk::Key::KP_8 => Some(7),
        gdk::Key::_9 | gdk::Key::KP_9 => Some(8),
        _ => None,
    }
}

fn is_alt_key(key: gdk::Key) -> bool {
    key == gdk::Key::Alt_L || key == gdk::Key::Alt_R
}

fn sidebar_nav_button(icon: &str, label: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::new();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let image = icon_image(icon, 16);
    image.add_css_class("sidebar-nav-icon");
    image.set_halign(gtk::Align::Center);
    image.set_valign(gtk::Align::Center);
    let text = gtk::Label::new(Some(label));
    text.set_xalign(0.0);
    text.add_css_class("session-tab-title");
    row.append(&image);
    row.append(&text);
    button.set_child(Some(&row));
    button.set_tooltip_text(Some(tooltip));
    button.add_css_class("sidebar-nav");
    button
}

fn paperclip_icon(pixel_size: i32) -> gtk::DrawingArea {
    drawn_icon(pixel_size, draw_paperclip)
}

fn panel_icon(pixel_size: i32) -> gtk::DrawingArea {
    drawn_icon(pixel_size, draw_panel)
}

fn drawn_icon(
    pixel_size: i32,
    draw: fn(&gtk::DrawingArea, &cairo::Context, i32, i32, i32),
) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(pixel_size)
        .content_height(pixel_size)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .can_target(false)
        .build();
    area.set_draw_func(move |widget, cr, width, height| {
        draw(widget, cr, width, height, pixel_size);
    });
    area
}

fn draw_paperclip(
    widget: &gtk::DrawingArea,
    cr: &cairo::Context,
    width: i32,
    height: i32,
    pixel_size: i32,
) {
    let color = widget
        .parent()
        .map(|parent| parent.style_context().color())
        .unwrap_or_else(|| widget.style_context().color());
    cr.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha()),
    );
    let size = f64::from(pixel_size);
    let path_scale = 0.85;
    let s = size / 24.0 * path_scale;
    cr.save().ok();
    cr.translate(
        f64::from(width) / 2.0 - 12.2 * s,
        f64::from(height) / 2.0 - 12.0 * s,
    );
    cr.set_line_width(1.7 * (size / 24.0));
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.move_to(16.6 * s, 5.8 * s);
    cr.line_to(16.6 * s, 15.0 * s);
    cr.arc(12.2 * s, 15.0 * s, 4.4 * s, 0.0, std::f64::consts::PI);
    cr.line_to(7.8 * s, 7.4 * s);
    cr.arc(10.0 * s, 7.4 * s, 2.2 * s, std::f64::consts::PI, 0.0);
    cr.line_to(12.2 * s, 14.8 * s);
    let _ = cr.stroke();
    cr.restore().ok();
}

fn draw_panel(
    widget: &gtk::DrawingArea,
    cr: &cairo::Context,
    width: i32,
    height: i32,
    pixel_size: i32,
) {
    let color = widget
        .parent()
        .map(|parent| parent.style_context().color())
        .unwrap_or_else(|| widget.style_context().color());
    cr.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha()),
    );
    let size = f64::from(pixel_size);
    let s = size / 16.0;
    cr.save().ok();
    cr.translate(
        f64::from(width) / 2.0 - 8.0 * s,
        f64::from(height) / 2.0 - 8.0 * s,
    );
    cr.set_line_width(1.5 * s);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.rectangle(2.5 * s, 3.0 * s, 11.0 * s, 10.0 * s);
    let _ = cr.stroke();
    cr.move_to(6.5 * s, 3.0 * s);
    cr.line_to(6.5 * s, 13.0 * s);
    let _ = cr.stroke();
    cr.restore().ok();
}

fn chevron_down_icon(pixel_size: i32) -> gtk::DrawingArea {
    drawn_icon(pixel_size, draw_chevron_down)
}

fn draw_chevron_down(
    widget: &gtk::DrawingArea,
    cr: &cairo::Context,
    width: i32,
    height: i32,
    pixel_size: i32,
) {
    let color = widget
        .parent()
        .map(|parent| parent.style_context().color())
        .unwrap_or_else(|| widget.style_context().color());
    cr.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha()) * 0.75,
    );
    let size = f64::from(pixel_size);
    let s = size / 10.0;
    cr.save().ok();
    cr.translate(
        f64::from(width) / 2.0 - 5.0 * s,
        f64::from(height) / 2.0 - 5.0 * s,
    );
    cr.set_line_width(1.4 * s);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.move_to(2.0 * s, 3.5 * s);
    cr.line_to(5.0 * s, 6.5 * s);
    cr.line_to(8.0 * s, 3.5 * s);
    let _ = cr.stroke();
    cr.restore().ok();
}

fn fuzzy_score(query: &str, target: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let q_lower: Vec<char> = query.to_lowercase().chars().collect();
    let t_lower: Vec<char> = target.to_lowercase().chars().collect();
    let original: Vec<char> = target.chars().collect();

    let mut q_idx = 0;
    let mut score: i64 = 0;
    let mut prev_match_idx: Option<usize> = None;
    let mut first_match_idx: Option<usize> = None;

    for (t_idx, &t_ch) in t_lower.iter().enumerate() {
        if q_idx < q_lower.len() && t_ch == q_lower[q_idx] {
            if first_match_idx.is_none() {
                first_match_idx = Some(t_idx);
            }
            score += 10;

            if let Some(prev) = prev_match_idx {
                if prev + 1 == t_idx {
                    score += 15;
                }
            }

            if t_idx == 0 {
                score += 30;
            } else {
                let prev_ch = original[t_idx - 1];
                if matches!(prev_ch, ' ' | '-' | '_' | '/' | '.' | ':') {
                    score += 25;
                } else if prev_ch.is_lowercase() && original[t_idx].is_uppercase() {
                    score += 20;
                }
            }

            prev_match_idx = Some(t_idx);
            q_idx += 1;
        }
    }

    if q_idx < q_lower.len() {
        return None;
    }

    let t_str = target.to_lowercase();
    let q_str = query.to_lowercase();
    if let Some(idx) = t_str.find(&q_str) {
        score += 50;
        if idx == 0 {
            score += 25;
        }
    }

    if let (Some(first), Some(last)) = (first_match_idx, prev_match_idx) {
        let span = last.saturating_sub(first);
        score -= span as i64;
    }

    score -= (t_lower.len() as i64) / 4;

    Some(score)
}

fn set_button_icon(button: &gtk::Button, name: &str, pixel_size: i32) {
    if let Some(image) = button.child().and_downcast::<gtk::Image>() {
        image.set_icon_name(Some(name));
        image.set_pixel_size(pixel_size);
        return;
    }
    button.set_child(Some(&icon_image(name, pixel_size)));
}

pub fn launch(
    application: &gtk::Application,
    server: Option<String>,
    username: Option<String>,
    password: Option<String>,
    cf_access_client_id: Option<String>,
    cf_access_client_secret: Option<String>,
    preview: bool,
) {
    register_icons();
    let state_path = default_path();
    let (persisted, persistence_warning, persistence_error) = if preview {
        (PersistedState::default(), None, None)
    } else {
        match PersistedState::load(&state_path) {
            Ok((persisted, warning)) => (persisted, warning, None),
            Err(error) => (PersistedState::default(), None, Some(error.to_string())),
        }
    };
    let base_url = server.unwrap_or_else(|| persisted.connection.server.clone());
    let (cloudflare_access, credential_warning) = if preview {
        (None, None)
    } else {
        let load_stored_cloudflare = persisted.connection.cloudflare_access
            && base_url.trim_end_matches('/') == persisted.connection.server.trim_end_matches('/');
        initial_cloudflare_credentials(
            &base_url,
            load_stored_cloudflare,
            cf_access_client_id,
            cf_access_client_secret,
        )
    };
    let config = ApiConfig {
        base_url,
        username: username.unwrap_or_else(|| persisted.connection.username.clone()),
        password,
        cloudflare_access,
    };
    install_css();
    let widgets = build_widgets(application);
    if preview {
        widgets.window.set_title(Some("OpenCode Preview"));
    }
    let (api, events, server_key) = if preview {
        ApiHandle::preview()
    } else {
        match ApiHandle::start(config.clone()) {
            Ok(started) => started,
            Err(error) => {
                widgets.status.set_label(&error.to_string());
                widgets.status.add_css_class("error");
                widgets.window.present();
                return;
            }
        }
    };

    let persistence_writes_blocked = preview || persistence_error.is_some();
    let (had_server_state, server_state) = if preview {
        (true, crate::preview::server_state())
    } else {
        (
            persisted.servers.contains_key(&server_key),
            persisted
                .servers
                .get(&server_key)
                .cloned()
                .unwrap_or_default(),
        )
    };
    let offline_busy = server_state.busy.clone();
    let state = restored_state(server_state);

    let controller = Rc::new(RefCell::new(Controller {
        api,
        events: events.clone(),
        connection_config: config,
        connection_generation: 1,
        server_key,
        state_path,
        persisted,
        persistence_warning,
        persistence_error,
        credential_warning,
        persistence_writes_blocked,
        had_server_state,
        offline_busy,
        state,
        widgets,
        rendered_session: None,
        rendered_rows: Vec::new(),
        transcript_heights: Vec::new(),
        transcript_visible: Vec::new(),
        transcript_pool: Vec::new(),
        transcript_at_bottom: true,
        transcript_scroll_value: 0.0,
        transcript_scroll_generation: 0,
        transcript_edge_refresh_scheduled: false,
        transcript_edge_remeasure: false,
        transcript_layout_width: 0,
        current_models: Vec::new(),
        current_variants: Vec::new(),
        controls_updating: false,
        pending_events: Vec::new(),
        event_flush_scheduled: false,
        session_events_during_bootstrap: HashMap::new(),
        session_removals_during_bootstrap: HashMap::new(),
        status_events_during_bootstrap: HashMap::new(),
        resolved_requests_during_bootstrap: HashSet::new(),
        message_events_during_load: HashMap::new(),
        replacing_messages: HashSet::new(),
        message_reload_pending: HashSet::new(),
        message_load_errors: HashMap::new(),
        models_reload_pending: HashSet::new(),
        model_load_errors: HashMap::new(),
        bootstrap_pending: true,
        bootstrap_reload_pending: false,
        bootstrap_dialogs_at_start: HashSet::new(),
        bootstrap_retry_token: 1,
        bootstrap_retry_delay: BOOTSTRAP_RETRY_MIN,
        self_weak: Weak::new(),
        dialogs: HashMap::new(),
        pending_actions: HashSet::new(),
        composer_prompts: Vec::new(),
        shown_composer_prompt: None,
        app_modal: None,
        app_modal_focus: None,
        session_picker: None,
        rename_session_id: None,
        next_session_request_id: 0,
        pending_session_request: None,
        pending_rename_request: None,
        next_prompt_request_id: 0,
        connected_once: false,
        event_connected: false,
        tab_shortcut_hint: false,
    }));
    controller.borrow_mut().self_weak = Rc::downgrade(&controller);
    {
        let this = controller.borrow();
        this.widgets.status.set_tooltip_text(Some(&this.server_key));
    }

    wire_callbacks(&controller);
    let quit = gio::SimpleAction::new("quit", None);
    let window = controller.borrow().widgets.window.clone();
    quit.connect_activate(move |_, _| {
        window.close();
    });
    application.add_action(&quit);
    application.set_accels_for_action("app.quit", &["<Control>q"]);

    let activate_session =
        gio::SimpleAction::new("activate-session", Some(glib::VariantTy::STRING));
    let weak = Rc::downgrade(&controller);
    activate_session.connect_activate(move |_, target| {
        let Some(controller) = weak.upgrade() else {
            return;
        };
        let (window, composer) = {
            let this = controller.borrow();
            (this.widgets.window.clone(), this.widgets.composer.clone())
        };
        window.present();
        gtk::prelude::GtkWindowExt::set_focus(&window, Some(&composer));
        if let Some(session_id) = target.and_then(|v| v.get::<String>()) {
            Controller::open_tab(&controller, &session_id);
        }
    });
    application.add_action(&activate_session);
    Controller::refresh_all(&controller);
    {
        let widgets = &controller.borrow().widgets;
        gtk::prelude::GtkWindowExt::set_focus(&widgets.window, Some(&widgets.new_button));
        widgets.window.present();
    }
    controller.borrow().api.send(Command::Bootstrap);

    let close_controller = Rc::downgrade(&controller);
    controller
        .borrow()
        .widgets
        .window
        .connect_close_request(move |_| {
            if let Some(controller) = close_controller.upgrade() {
                let mut controller = controller.borrow_mut();
                controller.persist_state();
                controller.events.close();
            }
            glib::Propagation::Proceed
        });
    start_event_loop(&controller, events, 1);
}

fn initial_cloudflare_credentials(
    server: &str,
    load_stored: bool,
    client_id: Option<String>,
    client_secret: Option<String>,
) -> (Option<CloudflareAccessCredentials>, Option<String>) {
    let result = match (client_id, client_secret) {
        (Some(client_id), Some(client_secret)) => {
            CloudflareAccessCredentials::new(client_id, client_secret).map(Some)
        }
        (None, None) if load_stored => credentials::load(server),
        (None, None) => Ok(None),
        _ => Err(anyhow::anyhow!(
            "Cloudflare Access client ID and secret must be provided together"
        )),
    };
    match result {
        Ok(Some(credentials)) => (Some(credentials), None),
        Ok(None) if load_stored => (
            None,
            Some("Cloudflare Access credentials were not found in the system keyring".into()),
        ),
        Ok(None) => (None, None),
        Err(error) => (None, Some(error.to_string())),
    }
}

fn configured_cloudflare_credentials(
    current: &ApiConfig,
    server: &str,
    client_id: &str,
    client_secret: &str,
) -> anyhow::Result<Option<CloudflareAccessCredentials>> {
    let client_id = client_id.trim();
    let client_secret = client_secret.trim();
    if client_id.is_empty() && client_secret.is_empty() {
        return Ok(None);
    }
    if client_secret.is_empty() {
        if same_server(&current.base_url, server) {
            if let Some(credentials) = current
                .cloudflare_access
                .as_ref()
                .filter(|credentials| credentials.client_id == client_id)
            {
                return Ok(Some(credentials.clone()));
            }
        }
        anyhow::bail!("Cloudflare Access client secret is required");
    }
    CloudflareAccessCredentials::new(client_id.to_owned(), client_secret.to_owned()).map(Some)
}

fn persist_cloudflare_credentials(current: &ApiConfig, next: &ApiConfig) -> anyhow::Result<()> {
    if let Some(credentials) = &next.cloudflare_access {
        credentials::save(&next.base_url, credentials)
    } else if current.cloudflare_access.is_some() && same_server(&current.base_url, &next.base_url)
    {
        credentials::remove(&next.base_url)
    } else {
        Ok(())
    }
}

fn same_server(left: &str, right: &str) -> bool {
    left.trim().trim_end_matches('/') == right.trim().trim_end_matches('/')
}

fn start_event_loop(
    controller: &Rc<RefCell<Controller>>,
    events: Receiver<UiEvent>,
    generation: u64,
) {
    let event_controller = controller.clone();
    glib::spawn_future_local(async move {
        while let Ok(event) = events.recv().await {
            if event_controller.borrow().connection_generation != generation {
                break;
            }
            Controller::handle_ui_event(&event_controller, event);
        }
    });
}

fn restored_state(server_state: ServerState) -> State {
    let mut state = State {
        tabs: server_state.tabs.iter().map(|tab| tab.id.clone()).collect(),
        active: server_state.active,
        selections: server_state.selections,
        unread: server_state.unread,
        ..State::default()
    };
    state
        .sessions
        .extend(server_state.tabs.into_iter().map(|tab| Session {
            id: tab.id,
            directory: tab.directory,
            title: tab.title,
            time: SessionTime {
                created: 0,
                updated: 0,
                archived: None,
            },
            parent_id: None,
            agent: None,
            model: None,
        }));
    state
}

fn build_widgets(application: &gtk::Application) -> Widgets {
    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .title("OpenCode")
        .default_width(1180)
        .default_height(820)
        .resizable(true)
        .build();

    let header = gtk::HeaderBar::new();
    let sidebar_toggle = gtk::Button::new();
    sidebar_toggle.set_child(Some(&panel_icon(16)));
    sidebar_toggle.set_tooltip_text(Some("Hide sidebar (Ctrl+B)"));
    sidebar_toggle.add_css_class("flat");
    sidebar_toggle.add_css_class("sidebar-toggle");
    header.pack_start(&sidebar_toggle);
    let session_button = sidebar_nav_button(ICON_SESSIONS, "Tabs", "Search tabs (Ctrl+P)");
    let new_button = gtk::Button::new();
    let new_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let new_plus = icon_image(ICON_ADD, 12);
    new_plus.add_css_class("session-tab-status");
    new_plus.set_halign(gtk::Align::Center);
    new_plus.set_valign(gtk::Align::Center);
    let new_label = gtk::Label::new(Some("New session"));
    new_label.set_xalign(0.0);
    new_label.add_css_class("session-tab-title");
    new_row.append(&new_plus);
    new_row.append(&new_label);
    new_button.set_child(Some(&new_row));
    new_button.set_tooltip_text(Some("New session (Ctrl+T)"));
    new_button.add_css_class("sidebar-new-session");
    let settings_button =
        sidebar_nav_button(ICON_SETTINGS, "Settings", "Server connection (Ctrl+,)");
    let title = gtk::Label::new(Some("OpenCode"));
    title.add_css_class("app-title");
    let status = gtk::Label::new(Some("Connecting"));
    status.add_css_class("connection-status");
    let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    title_box.append(&title);
    title_box.append(&status);
    header.set_title_widget(Some(&title_box));
    window.set_titlebar(Some(&header));

    let root = gtk::Paned::new(gtk::Orientation::Horizontal);
    root.set_position(270);
    root.set_resize_start_child(false);
    root.set_shrink_start_child(false);
    root.set_hexpand(true);
    root.set_vexpand(true);
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar.add_css_class("tab-strip");
    new_button.set_margin_start(8);
    new_button.set_margin_end(8);
    new_button.set_margin_top(8);
    new_button.set_margin_bottom(0);
    sidebar.append(&new_button);
    let tab_bar = gtk::Box::new(gtk::Orientation::Vertical, 2);
    tab_bar.add_css_class("session-tabs");
    tab_bar.set_margin_start(8);
    tab_bar.set_margin_end(8);
    tab_bar.set_margin_top(2);
    tab_bar.set_margin_bottom(8);
    let tab_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_width(250)
        .max_content_width(280)
        .vexpand(true)
        .kinetic_scrolling(false)
        .child(&tab_bar)
        .build();
    sidebar.append(&tab_scroll);
    let nav = gtk::Box::new(gtk::Orientation::Vertical, 2);
    nav.add_css_class("sidebar-nav-footer");
    let nav_rule = gtk::Separator::new(gtk::Orientation::Horizontal);
    nav_rule.add_css_class("sidebar-nav-separator");
    nav.append(&nav_rule);
    nav.append(&session_button);
    nav.append(&settings_button);
    sidebar.append(&nav);
    root.set_start_child(Some(&sidebar));
    let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main.set_hexpand(true);
    main.set_vexpand(true);

    let load_earlier = gtk::Button::with_label("Load earlier messages");
    load_earlier.add_css_class("flat");
    load_earlier.set_visible(false);

    let transcript_spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    transcript_spacer.set_hexpand(true);
    transcript_spacer.set_can_target(false);
    let transcript_tail = gtk::Box::new(gtk::Orientation::Vertical, 0);
    transcript_tail.set_hexpand(true);
    transcript_tail.set_can_target(false);
    let transcript = gtk::Box::new(gtk::Orientation::Vertical, 0);
    transcript.set_hexpand(true);
    transcript.set_overflow(gtk::Overflow::Hidden);
    let transcript_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    transcript_column.add_css_class("transcript");
    transcript_column.set_hexpand(true);
    transcript_column.set_overflow(gtk::Overflow::Hidden);
    transcript_column.append(&transcript_spacer);
    transcript_column.append(&transcript);
    transcript_column.append(&transcript_tail);
    let transcript_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .overlay_scrolling(false)
        .vexpand(true)
        .hexpand(true)
        .propagate_natural_height(false)
        .min_content_height(0)
        .child(&transcript_column)
        .build();
    let transcript_spinner = gtk::Spinner::new();
    let transcript_status_label = gtk::Label::new(Some("Open a session to begin"));
    let transcript_status = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    transcript_status.add_css_class("transcript-status");
    transcript_status.set_halign(gtk::Align::Center);
    transcript_status.set_hexpand(true);
    transcript_status.set_can_target(false);
    transcript_status.append(&transcript_spinner);
    transcript_status.append(&transcript_status_label);
    let sticky_message = gtk::Box::new(gtk::Orientation::Vertical, 6);
    sticky_message.add_css_class("message-row");
    sticky_message.add_css_class("user-message");
    sticky_message.add_css_class("sticky-message");
    sticky_message.set_overflow(gtk::Overflow::Hidden);
    sticky_message.set_halign(gtk::Align::Fill);
    sticky_message.set_valign(gtk::Align::Start);
    sticky_message.set_can_target(true);
    sticky_message.set_visible(false);
    let sticky_message_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    sticky_message_header.add_css_class("message-header");
    let sticky_message_role = gtk::Label::new(Some("YOU"));
    sticky_message_role.set_xalign(0.0);
    sticky_message_role.set_hexpand(true);
    sticky_message_role.add_css_class("message-role");
    let sticky_message_time = gtk::Label::new(None);
    sticky_message_time.set_xalign(1.0);
    sticky_message_time.add_css_class("message-time");
    sticky_message_header.append(&sticky_message_role);
    sticky_message_header.append(&sticky_message_time);
    let sticky_message_body = gtk::Label::new(None);
    sticky_message_body.set_xalign(0.0);
    sticky_message_body.set_yalign(0.0);
    sticky_message_body.set_wrap(true);
    sticky_message_body.set_wrap_mode(pango::WrapMode::WordChar);
    sticky_message_body.add_css_class("message-content");
    sticky_message_body.add_css_class("message-plain-text");
    let sticky_message_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(180)
        .child(&sticky_message_body)
        .build();
    sticky_message_scroll.add_css_class("sticky-message-scroll");
    let transcript_user_scrolling = Rc::new(Cell::new(false));
    let sticky_scroll_controller =
        gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    sticky_scroll_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let sticky_adjustment = sticky_message_scroll.vadjustment();
    let transcript_adjustment = transcript_scroll.vadjustment();
    let user_scrolling = transcript_user_scrolling.clone();
    sticky_scroll_controller.connect_scroll(move |_, _, delta| {
        if !scroll_adjustment_by(&sticky_adjustment, delta) {
            user_scrolling.set(true);
            scroll_adjustment_by(&transcript_adjustment, delta);
        }
        glib::Propagation::Stop
    });
    sticky_message.add_controller(sticky_scroll_controller);
    sticky_message.append(&sticky_message_header);
    sticky_message.append(&sticky_message_scroll);

    let overlay = gtk::Overlay::new();
    overlay.set_vexpand(true);
    overlay.set_hexpand(true);
    overlay.set_child(Some(&transcript_scroll));
    overlay.add_overlay(&sticky_message);

    let session_header_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    session_header_bar.add_css_class("session-header-bar");
    session_header_bar.set_visible(false);

    let session_header_status = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    session_header_status.set_size_request(14, 14);
    session_header_status.set_halign(gtk::Align::Start);
    session_header_status.set_valign(gtk::Align::Center);

    let session_header_title = gtk::Label::new(Some("OpenCode"));
    session_header_title.add_css_class("session-header-title");
    session_header_title.set_xalign(0.0);
    session_header_title.set_hexpand(true);
    session_header_title.set_ellipsize(pango::EllipsizeMode::End);

    let session_header_hint = gtk::Label::new(Some("Ctrl+P to switch"));
    session_header_hint.add_css_class("session-header-hint");
    session_header_hint.set_xalign(1.0);
    session_header_hint.set_valign(gtk::Align::Center);

    session_header_bar.append(&session_header_status);
    session_header_bar.append(&session_header_title);
    session_header_bar.append(&session_header_hint);

    let conversation = gtk::Box::new(gtk::Orientation::Vertical, 0);
    conversation.set_vexpand(true);
    conversation.append(&session_header_bar);
    conversation.append(&load_earlier);
    conversation.append(&overlay);
    conversation.append(&transcript_status);
    main.append(&conversation);

    let composer = gtk::TextView::new();
    composer.set_wrap_mode(gtk::WrapMode::WordChar);
    composer.set_accepts_tab(false);
    composer.set_top_margin(10);
    composer.set_bottom_margin(10);
    composer.set_left_margin(12);
    composer.set_right_margin(12);
    composer.add_css_class("composer-input");
    let composer_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(72)
        .max_content_height(220)
        .propagate_natural_height(true)
        .vexpand(false)
        .child(&composer)
        .build();

    let attachment_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    attachment_box.set_visible(false);
    let attach_button = gtk::Button::new();
    attach_button.set_child(Some(&paperclip_icon(COMPOSER_ICON_PX)));
    attach_button.set_tooltip_text(Some("Attach files (Ctrl+U)"));
    attach_button.add_css_class("composer-action");

    let model_button = gtk::Button::new();
    let model_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let model_button_label = gtk::Label::new(Some("Loading models..."));
    model_button_label.set_xalign(0.0);
    model_button_label.set_hexpand(true);
    model_button_label.set_ellipsize(pango::EllipsizeMode::End);
    model_button_label.add_css_class("composer-menu-title");

    let model_chevron = chevron_down_icon(10);
    model_chevron.add_css_class("composer-menu-arrow");
    model_chevron.set_halign(gtk::Align::End);
    model_chevron.set_valign(gtk::Align::Center);

    model_row.append(&model_button_label);
    model_row.append(&model_chevron);
    model_button.set_child(Some(&model_row));
    model_button.add_css_class("composer-menu");
    model_button.set_tooltip_text(Some("Select model (Ctrl+M)"));
    model_button.set_sensitive(false);
    model_button.set_hexpand(true);

    let model_popover = gtk::Popover::new();
    model_popover.set_parent(&model_button);
    model_popover.set_position(gtk::PositionType::Top);
    model_popover.set_offset(0, -6);
    model_popover.set_autohide(true);
    model_popover.add_css_class("model-picker-popover");

    let popover_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    popover_box.set_margin_start(8);
    popover_box.set_margin_end(8);
    popover_box.set_margin_top(8);
    popover_box.set_margin_bottom(8);
    popover_box.set_size_request(340, -1);

    let model_search = gtk::SearchEntry::new();
    model_search.set_placeholder_text(Some("Search models (fuzzy)..."));
    model_search.add_css_class("model-picker-search");
    popover_box.append(&model_search);

    let model_list = gtk::ListBox::new();
    model_list.set_selection_mode(gtk::SelectionMode::Single);
    model_list.add_css_class("model-picker-list");

    let model_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(280)
        .min_content_height(100)
        .child(&model_list)
        .build();
    popover_box.append(&model_scroll);

    model_popover.set_child(Some(&popover_box));
    let model_filtered_indices = Rc::new(RefCell::new(Vec::new()));

    let variant_button = gtk::Button::new();
    let variant_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let variant_button_label = gtk::Label::new(Some("Default"));
    variant_button_label.set_xalign(0.0);
    variant_button_label.set_hexpand(true);
    variant_button_label.set_ellipsize(pango::EllipsizeMode::End);
    variant_button_label.add_css_class("composer-menu-title");

    let variant_chevron = chevron_down_icon(10);
    variant_chevron.add_css_class("composer-menu-arrow");
    variant_chevron.set_halign(gtk::Align::End);
    variant_chevron.set_valign(gtk::Align::Center);

    variant_row.append(&variant_button_label);
    variant_row.append(&variant_chevron);
    variant_button.set_child(Some(&variant_row));
    variant_button.add_css_class("composer-menu");
    variant_button.set_tooltip_text(Some("Reasoning level (Ctrl+/)"));
    variant_button.set_sensitive(false);

    let variant_popover = gtk::Popover::new();
    variant_popover.set_parent(&variant_button);
    variant_popover.set_position(gtk::PositionType::Top);
    variant_popover.set_offset(0, -6);
    variant_popover.set_autohide(true);
    variant_popover.add_css_class("model-picker-popover");

    let variant_popover_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    variant_popover_box.set_margin_start(8);
    variant_popover_box.set_margin_end(8);
    variant_popover_box.set_margin_top(8);
    variant_popover_box.set_margin_bottom(8);
    variant_popover_box.set_size_request(240, -1);

    let variant_search = gtk::SearchEntry::new();
    variant_search.set_placeholder_text(Some("Search levels (fuzzy)..."));
    variant_search.add_css_class("model-picker-search");
    variant_popover_box.append(&variant_search);

    let variant_list = gtk::ListBox::new();
    variant_list.set_selection_mode(gtk::SelectionMode::Single);
    variant_list.add_css_class("model-picker-list");

    let variant_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(240)
        .min_content_height(80)
        .child(&variant_list)
        .build();
    variant_popover_box.append(&variant_scroll);

    variant_popover.set_child(Some(&variant_popover_box));
    let variant_filtered_indices = Rc::new(RefCell::new(Vec::new()));

    let context_usage = gtk::Label::new(None);
    context_usage.add_css_class("composer-usage");
    context_usage.set_xalign(0.0);
    context_usage.set_hexpand(true);
    context_usage.set_ellipsize(pango::EllipsizeMode::End);
    context_usage.set_valign(gtk::Align::Center);
    context_usage.set_visible(false);
    let send_button = icon_button(ICON_SEND, COMPOSER_ICON_PX);
    send_button.add_css_class("suggested-action");
    send_button.add_css_class("composer-action");
    send_button.set_tooltip_text(Some("Send prompt"));
    send_button.set_sensitive(false);

    let composer_controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    composer_controls.set_overflow(gtk::Overflow::Hidden);
    composer_controls.append(&attach_button);
    composer_controls.append(&model_button);
    composer_controls.append(&variant_button);
    composer_controls.append(&context_usage);
    composer_controls.append(&send_button);

    let composer_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    composer_box.set_margin_start(14);
    composer_box.set_margin_end(14);
    composer_box.set_margin_top(10);
    composer_box.set_margin_bottom(12);
    composer_box.append(&attachment_box);
    composer_box.append(&composer_scroll);
    composer_box.append(&composer_controls);
    let composer_frame = gtk::Frame::new(None);
    composer_frame.add_css_class("composer-frame");
    composer_frame.set_hexpand(true);
    composer_frame.set_vexpand(false);
    composer_frame.set_valign(gtk::Align::End);
    composer_frame.set_child(Some(&composer_box));
    let prompt_host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let prompt_frame = gtk::Frame::new(None);
    prompt_frame.add_css_class("composer-frame");
    prompt_frame.add_css_class("composer-prompt");
    prompt_frame.set_hexpand(true);
    prompt_frame.set_vexpand(false);
    prompt_frame.set_valign(gtk::Align::End);
    prompt_frame.set_child(Some(&prompt_host));
    let composer_stack = gtk::Stack::new();
    composer_stack.set_hexpand(true);
    composer_stack.set_vexpand(false);
    composer_stack.set_valign(gtk::Align::End);
    composer_stack.set_margin_start(18);
    composer_stack.set_margin_end(18);
    composer_stack.set_margin_bottom(16);
    composer_stack.add_named(&composer_frame, Some("composer"));
    composer_stack.add_named(&prompt_frame, Some("prompt"));
    composer_stack.set_visible_child_name("composer");
    main.append(&composer_stack);
    root.set_end_child(Some(&main));

    let new_session_overlay = gtk::Box::new(gtk::Orientation::Vertical, 0);
    new_session_overlay.add_css_class("modal-backdrop");
    new_session_overlay.set_hexpand(true);
    new_session_overlay.set_vexpand(true);
    new_session_overlay.set_halign(gtk::Align::Fill);
    new_session_overlay.set_valign(gtk::Align::Fill);
    new_session_overlay.set_visible(false);

    let modal_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    modal_card.add_css_class("new-session-palette");
    modal_card.set_overflow(gtk::Overflow::Hidden);
    modal_card.set_halign(gtk::Align::Center);
    modal_card.set_valign(gtk::Align::Center);
    modal_card.set_size_request(350, 310);

    let new_session_search = gtk::Entry::new();
    new_session_search.set_placeholder_text(Some("Search projects..."));
    new_session_search.add_css_class("new-session-search");

    let new_session_list = gtk::ListBox::new();
    new_session_list.set_selection_mode(gtk::SelectionMode::Single);
    new_session_list.add_css_class("new-session-list");

    let new_session_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(false)
        .min_content_height(264)
        .vexpand(true)
        .child(&new_session_list)
        .build();

    modal_card.append(&new_session_search);
    modal_card.append(&new_session_scroll);

    let top_spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    top_spacer.set_vexpand(true);
    top_spacer.set_can_target(false);

    let bottom_spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    bottom_spacer.set_vexpand(true);
    bottom_spacer.set_can_target(false);

    new_session_overlay.append(&top_spacer);
    new_session_overlay.append(&modal_card);
    new_session_overlay.append(&bottom_spacer);

    let app_modal_overlay = gtk::Box::new(gtk::Orientation::Vertical, 0);
    app_modal_overlay.add_css_class("modal-backdrop");
    app_modal_overlay.set_hexpand(true);
    app_modal_overlay.set_vexpand(true);
    app_modal_overlay.set_halign(gtk::Align::Fill);
    app_modal_overlay.set_valign(gtk::Align::Fill);
    app_modal_overlay.set_visible(false);

    let app_modal_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    app_modal_card.add_css_class("app-modal-palette");
    app_modal_card.set_overflow(gtk::Overflow::Hidden);
    app_modal_card.set_halign(gtk::Align::Center);
    app_modal_card.set_valign(gtk::Align::Center);

    let app_modal_top_spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    app_modal_top_spacer.set_vexpand(true);
    app_modal_top_spacer.set_can_target(false);
    let app_modal_bottom_spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    app_modal_bottom_spacer.set_vexpand(true);
    app_modal_bottom_spacer.set_can_target(false);
    app_modal_overlay.append(&app_modal_top_spacer);
    app_modal_overlay.append(&app_modal_card);
    app_modal_overlay.append(&app_modal_bottom_spacer);

    let new_session_filtered_paths = Rc::new(RefCell::new(Vec::new()));

    let window_overlay = gtk::Overlay::new();
    window_overlay.set_child(Some(&root));
    window_overlay.add_overlay(&new_session_overlay);
    window_overlay.set_measure_overlay(&new_session_overlay, false);
    window_overlay.set_clip_overlay(&new_session_overlay, false);
    window_overlay.add_overlay(&app_modal_overlay);
    window_overlay.set_measure_overlay(&app_modal_overlay, false);
    window_overlay.set_clip_overlay(&app_modal_overlay, false);
    window.set_child(Some(&window_overlay));

    Widgets {
        window,
        sidebar,
        sidebar_toggle,
        session_button,
        new_button,
        settings_button,
        status,
        session_header_bar,
        session_header_status,
        session_header_title,
        tab_bar,
        transcript,
        transcript_spacer,
        transcript_tail,
        transcript_scroll,
        sticky_message,
        sticky_message_scroll,
        sticky_message_body,
        sticky_message_time,
        transcript_status,
        transcript_spinner,
        transcript_status_label,
        load_earlier,
        composer_stack,
        prompt_host,
        composer,
        attachment_box,
        attach_button,
        model_button,
        model_button_label,
        model_popover,
        model_search,
        model_list,
        model_filtered_indices,
        variant_button,
        variant_button_label,
        variant_popover,
        variant_search,
        variant_list,
        variant_filtered_indices,
        new_session_overlay,
        new_session_card: modal_card,
        app_modal_overlay,
        app_modal_card,
        new_session_search,
        new_session_list,
        new_session_filtered_paths,
        context_usage,
        send_button,
        transcript_user_scrolling,
        tab_dnd: Rc::new(RefCell::new(TabDnd::default())),
    }
}

fn transcript_row_widget() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 6);
    row.add_css_class("message-row");
    row.set_overflow(gtk::Overflow::Hidden);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.add_css_class("message-header");
    let role = gtk::Label::new(None);
    role.set_xalign(0.0);
    role.set_hexpand(true);
    role.add_css_class("message-role");
    let time = gtk::Label::new(None);
    time.set_xalign(1.0);
    time.add_css_class("message-time");
    header.append(&role);
    header.append(&time);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_hexpand(true);
    content.add_css_class("message-content");
    row.append(&header);
    row.append(&content);
    row
}

fn bind_transcript_row(row: &gtk::Box, value: &str, index: u32) {
    let Some(header) = row.first_child().and_downcast::<gtk::Box>() else {
        return;
    };
    let Some(role) = header.first_child().and_downcast::<gtk::Label>() else {
        return;
    };
    let Some(time) = header.last_child().and_downcast::<gtk::Label>() else {
        return;
    };
    let Some(content) = row.last_child().and_downcast::<gtk::Box>() else {
        return;
    };
    let parsed = serde_json::from_str::<TranscriptRow>(value).ok();
    let (role_text, body, images, timestamp) = parsed
        .as_ref()
        .map(|row| {
            (
                row.role.as_str(),
                row.body.as_str(),
                row.images.as_slice(),
                row.time,
            )
        })
        .unwrap_or_else(|| {
            let (role, body) = value.split_once('\n').unwrap_or(("AGENT", value));
            (role, body, &[], 0)
        });
    role.set_label(role_text);
    let time_text = display_local_timestamp(timestamp).unwrap_or_default();
    time.set_label(&time_text);
    time.set_visible(!time_text.is_empty());
    clear_box(&content);
    let kind = parsed.as_ref().map(|row| row.kind.as_str()).unwrap_or("");
    if kind == "error" {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
        card.add_css_class("message-error-card");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.add_css_class("message-error-header");
        let icon = gtk::Label::new(Some("⚠"));
        icon.add_css_class("message-error-icon");
        let title = gtk::Label::new(Some("Error"));
        title.add_css_class("message-error-title");
        title.set_xalign(0.0);
        header.append(&icon);
        header.append(&title);

        let body_label = gtk::Label::new(Some(body));
        body_label.set_xalign(0.0);
        body_label.set_wrap(true);
        body_label.set_wrap_mode(pango::WrapMode::WordChar);
        body_label.set_selectable(true);
        body_label.add_css_class("message-error-body");

        card.append(&header);
        card.append(&body_label);
        content.append(&card);
    } else if role_text == "YOU" {
        content.append(&markdown::render_plain(body, "message-plain-text"));
    } else {
        markdown::render_into(&content, body);
    }
    if body.contains("Attached:") && images.is_empty() {
        debug_log("attached text without images[]");
    }
    for image in images {
        match inline_image_texture(image) {
            Some(texture) => {
                let picture = gtk::Picture::for_paintable(&texture);
                picture.set_can_shrink(true);
                picture.set_halign(gtk::Align::Start);
                picture.add_css_class("message-image");
                let width = texture.width().clamp(1, 640);
                let height = (i64::from(texture.height().max(1)) * i64::from(width)
                    / i64::from(texture.width().max(1)))
                .clamp(1, 720) as i32;
                picture.set_size_request(-1, height);
                debug_log(format!(
                    "image ok prefix={} texture={}x{} request_h={}",
                    image_prefix(image),
                    texture.width(),
                    texture.height(),
                    height
                ));
                content.append(&picture);
            }
            None => debug_log(format!("image fail prefix={}", image_prefix(image))),
        }
    }
    row.remove_css_class("user-message");
    row.remove_css_class("assistant-message");
    row.remove_css_class("message-reasoning");
    row.remove_css_class("message-error-row");
    row.add_css_class(if role_text == "YOU" {
        "user-message"
    } else {
        "assistant-message"
    });
    if kind == "reasoning" {
        row.add_css_class("message-reasoning");
    } else if kind == "error" {
        row.add_css_class("message-error-row");
    }
    row.set_widget_name(&format!("row-{index}"));
}

fn inline_image_texture(url: &str) -> Option<gdk::Texture> {
    if let Some(texture) = data_url_texture(url) {
        return Some(texture);
    }
    let path = url.strip_prefix("file://").unwrap_or(url);
    if path.starts_with('/') {
        return gdk::Texture::from_filename(path).ok();
    }
    None
}

fn data_url_texture(url: &str) -> Option<gdk::Texture> {
    let (metadata, encoded) = url.strip_prefix("data:")?.split_once(',')?;
    if !metadata.starts_with("image/") || !metadata.ends_with(";base64") {
        return None;
    }
    if encoded.len() > MAX_INLINE_IMAGE_BYTES.div_ceil(3) * 4 {
        return None;
    }
    let bytes = BASE64.decode(encoded).ok()?;
    (bytes.len() <= MAX_INLINE_IMAGE_BYTES)
        .then(|| gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok())
        .flatten()
}

fn image_prefix(url: &str) -> String {
    url.chars().take(48).collect()
}

fn debug_logging_enabled() -> bool {
    static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        if std::env::var_os("OPENCODE_GTK_DEBUG").is_some() {
            ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    });
    ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

fn debug_log(message: impl AsRef<str>) {
    if !debug_logging_enabled() {
        return;
    }
    let line = format!("{} {}\n", unix_millis(), message.as_ref());
    eprint!("{line}");
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/opencode-gtk-debug.log")
    {
        let _ = file.write_all(line.as_bytes());
    }
}

fn clipboard_attachment_dir() -> PathBuf {
    std::env::temp_dir().join(format!("opencode-gtk-clipboard-{}", std::process::id()))
}

fn save_clipboard_texture(texture: &gdk::Texture) -> Result<PathBuf, String> {
    let directory = clipboard_attachment_dir();
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let path = directory.join(format!("clipboard-{timestamp}.png"));
    texture
        .save_to_png(&path)
        .map_err(|error| error.to_string())?;
    Ok(path)
}

fn remove_clipboard_attachment(path: &PathBuf) {
    if path.parent() != Some(clipboard_attachment_dir().as_path()) {
        return;
    }
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir(clipboard_attachment_dir());
}

fn remove_pending_clipboard_attachments(pending: Option<PendingPrompt>) {
    if let Some(pending) = pending {
        pending
            .draft
            .attachments
            .iter()
            .for_each(remove_clipboard_attachment);
    }
}

fn wire_callbacks(controller: &Rc<RefCell<Controller>>) {
    let transcript_adjustment = controller.borrow().widgets.transcript_scroll.vadjustment();
    let user_scrolling = controller
        .borrow()
        .widgets
        .transcript_user_scrolling
        .clone();
    let scroll = gtk::EventControllerScroll::new(
        gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::KINETIC,
    );
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    let flag = user_scrolling.clone();
    scroll.connect_scroll(move |_, _, _| {
        flag.set(true);
        glib::Propagation::Proceed
    });
    controller
        .borrow()
        .widgets
        .transcript_scroll
        .add_controller(scroll);
    if let Ok(range) = controller
        .borrow()
        .widgets
        .transcript_scroll
        .vscrollbar()
        .downcast::<gtk::Range>()
    {
        let flag = user_scrolling.clone();
        range.connect_change_value(move |_, _, _| {
            flag.set(true);
            glib::Propagation::Proceed
        });
    }
    let weak = Rc::downgrade(controller);
    transcript_adjustment.connect_value_changed(move |adjustment| {
        let Some(controller) = weak.upgrade() else {
            return;
        };
        let Ok(mut controller) = controller.try_borrow_mut() else {
            return;
        };
        let at_bottom = adjustment_at_bottom(adjustment);
        let previous = controller.transcript_scroll_value;
        let scrolled_up = adjustment.value() + BOTTOM_EPSILON < previous;
        let jump = (adjustment.value() - previous).abs();
        controller.transcript_scroll_value = adjustment.value();
        if jump > 24.0 && debug_logging_enabled() {
            debug_log(format!(
                "scroll jump={jump:.1} value={:.1} page={:.1} upper={:.1} pinned={} user={}",
                adjustment.value(),
                adjustment.page_size(),
                adjustment.upper(),
                controller.transcript_at_bottom,
                controller.widgets.transcript_user_scrolling.get()
            ));
        }
        if controller.widgets.transcript_user_scrolling.get() {
            let (pinned, invalidate) =
                apply_user_bottom_pin(controller.transcript_at_bottom, at_bottom, scrolled_up);
            controller.transcript_at_bottom = pinned;
            if invalidate {
                controller.transcript_scroll_generation += 1;
            }
            if pinned && !at_bottom {
                scroll_adjustment_to_bottom(adjustment);
            }
            let flag = controller.widgets.transcript_user_scrolling.clone();
            glib::idle_add_local_once(move || {
                flag.set(false);
            });
        } else if controller.transcript_at_bottom && !at_bottom {
            if distance_from_bottom(adjustment) > 40.0 {
                scroll_adjustment_to_bottom(adjustment);
            }
        } else if at_bottom {
            controller.transcript_at_bottom = true;
        }
        controller.refresh_load_earlier_visibility();
        if controller.widgets.transcript_user_scrolling.get() || !controller.transcript_at_bottom {
            controller.queue_transcript_relayout(false);
        }
    });

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .sidebar_toggle
        .connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                controller.borrow_mut().toggle_sidebar();
            }
        });

    let weak = Rc::downgrade(controller);
    let header_click = gtk::GestureClick::new();
    header_click.set_button(1);
    header_click.connect_released(move |_, _, _, _| {
        if let Some(controller) = weak.upgrade() {
            Controller::show_session_picker(&controller);
        }
    });
    controller
        .borrow()
        .widgets
        .session_header_bar
        .add_controller(header_click);

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .session_button
        .connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::show_session_picker(&controller);
            }
        });

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .new_button
        .connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::show_new_session(&controller);
            }
        });

    let weak = Rc::downgrade(controller);
    let backdrop_click = gtk::GestureClick::new();
    backdrop_click.set_button(1);
    backdrop_click.connect_pressed(move |_, _, x, y| {
        if let Some(controller) = weak.upgrade() {
            let this = controller.borrow();
            let overlay = this.widgets.new_session_overlay.clone();
            let card = this.widgets.new_session_card.clone();
            drop(this);
            if !pointer_hits_widget(&card, &overlay, x, y) {
                controller.borrow().close_new_session_overlay();
            }
        }
    });
    controller
        .borrow()
        .widgets
        .new_session_overlay
        .add_controller(backdrop_click);

    let weak = Rc::downgrade(controller);
    let app_modal_backdrop_click = gtk::GestureClick::new();
    app_modal_backdrop_click.set_button(1);
    app_modal_backdrop_click.connect_pressed(move |_, _, x, y| {
        if let Some(controller) = weak.upgrade() {
            let this = controller.borrow();
            let overlay = this.widgets.app_modal_overlay.clone();
            let card = this.widgets.app_modal_card.clone();
            drop(this);
            if !pointer_hits_widget(&card, &overlay, x, y) {
                controller.borrow_mut().close_app_modal();
            }
        }
    });
    controller
        .borrow()
        .widgets
        .app_modal_overlay
        .add_controller(app_modal_backdrop_click);

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .new_session_search
        .connect_changed(move |search| {
            if let Some(controller) = weak.upgrade() {
                Controller::filter_new_session_projects(&controller, search.text().as_str());
            }
        });

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .new_session_search
        .connect_activate(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::activate_current_new_session_selection(&controller);
            }
        });

    let weak = Rc::downgrade(controller);
    let new_session_search_keys = gtk::EventControllerKey::new();
    new_session_search_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    new_session_search_keys.connect_key_pressed(move |_, key, _, _| {
        let Some(controller) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let (list, search) = {
            let this = controller.borrow();
            (
                this.widgets.new_session_list.clone(),
                this.widgets.new_session_search.clone(),
            )
        };
        match key {
            gdk::Key::Down => {
                if let Some(selected) = list.selected_row() {
                    let next_idx = selected.index() + 1;
                    if let Some(next_row) = list.row_at_index(next_idx) {
                        list.select_row(Some(&next_row));
                        next_row.grab_focus();
                    } else {
                        selected.grab_focus();
                    }
                } else if let Some(first_row) = list.row_at_index(0) {
                    list.select_row(Some(&first_row));
                    first_row.grab_focus();
                }
                glib::Propagation::Stop
            }
            gdk::Key::Up => {
                if let Some(selected) = list.selected_row() {
                    let prev_idx = selected.index() - 1;
                    if prev_idx >= 0 {
                        if let Some(prev_row) = list.row_at_index(prev_idx) {
                            list.select_row(Some(&prev_row));
                            prev_row.grab_focus();
                        }
                    } else {
                        search.grab_focus();
                    }
                } else {
                    search.grab_focus();
                }
                glib::Propagation::Stop
            }
            gdk::Key::Return | gdk::Key::KP_Enter => {
                Controller::activate_current_new_session_selection(&controller);
                glib::Propagation::Stop
            }
            gdk::Key::Escape => {
                controller.borrow().close_new_session_overlay();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    controller
        .borrow()
        .widgets
        .new_session_search
        .add_controller(new_session_search_keys);

    let weak = Rc::downgrade(controller);
    let new_session_list_keys = gtk::EventControllerKey::new();
    new_session_list_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    new_session_list_keys.connect_key_pressed(move |_, key, _, _| {
        let Some(controller) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let (list, search) = {
            let this = controller.borrow();
            (
                this.widgets.new_session_list.clone(),
                this.widgets.new_session_search.clone(),
            )
        };
        match key {
            gdk::Key::Return | gdk::Key::KP_Enter => {
                Controller::activate_current_new_session_selection(&controller);
                glib::Propagation::Stop
            }
            gdk::Key::Escape => {
                controller.borrow().close_new_session_overlay();
                glib::Propagation::Stop
            }
            gdk::Key::Up => {
                if let Some(selected) = list.selected_row() {
                    if selected.index() <= 0 {
                        search.grab_focus();
                        return glib::Propagation::Stop;
                    }
                }
                glib::Propagation::Proceed
            }
            _ => glib::Propagation::Proceed,
        }
    });
    controller
        .borrow()
        .widgets
        .new_session_list
        .add_controller(new_session_list_keys);

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .new_session_list
        .connect_row_activated(move |_, row| {
            if let Some(controller) = weak.upgrade() {
                let row_idx = row.index() as usize;
                let path = controller
                    .borrow()
                    .widgets
                    .new_session_filtered_paths
                    .borrow()
                    .get(row_idx)
                    .cloned();
                if let Some(path) = path {
                    Controller::create_session_for_project(&controller, &path);
                }
            }
        });

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .settings_button
        .connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::show_settings(&controller, None);
            }
        });

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .send_button
        .connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::send_or_abort(&controller);
            }
        });

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .attach_button
        .connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::pick_attachments(&controller);
            }
        });

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .load_earlier
        .connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::load_earlier(&controller);
            }
        });

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .composer
        .buffer()
        .connect_changed(move |buffer| {
            let Some(controller) = weak.upgrade() else {
                return;
            };
            let Ok(mut controller) = controller.try_borrow_mut() else {
                return;
            };
            if controller.controls_updating {
                return;
            }
            let Some(active) = controller.state.active.clone() else {
                return;
            };
            controller.state.drafts.entry(active).or_default().text = buffer_text(buffer);
            controller.refresh_send_button();
        });

    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(controller);
    key.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gdk::Key::v && modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
            let clipboard = gdk::Display::default().map(|display| display.clipboard());
            if clipboard.is_some_and(|clipboard| {
                let formats = clipboard.formats();
                formats.contains_type(gdk::Texture::static_type())
                    || formats
                        .mime_types()
                        .iter()
                        .any(|mime| mime.starts_with("image/"))
            }) {
                if let Some(controller) = weak.upgrade() {
                    Controller::paste_clipboard_image(&controller);
                }
                return glib::Propagation::Stop;
            }
        }
        if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter)
            && !modifiers.contains(gdk::ModifierType::SHIFT_MASK)
        {
            if let Some(controller) = weak.upgrade() {
                Controller::send_if_idle(&controller);
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    controller.borrow().widgets.composer.add_controller(key);

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .model_button
        .connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::show_model_picker(&controller);
            }
        });

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .model_search
        .connect_search_changed(move |search| {
            if let Some(controller) = weak.upgrade() {
                Controller::filter_model_list(&controller, search.text().as_str());
            }
        });

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .model_search
        .connect_activate(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::activate_current_model_selection(&controller);
            }
        });

    let weak = Rc::downgrade(controller);
    let search_keys = gtk::EventControllerKey::new();
    search_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    search_keys.connect_key_pressed(move |_, key, _, _| {
        let Some(controller) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let (list, popover, composer, search) = {
            let this = controller.borrow();
            (
                this.widgets.model_list.clone(),
                this.widgets.model_popover.clone(),
                this.widgets.composer.clone(),
                this.widgets.model_search.clone(),
            )
        };
        match key {
            gdk::Key::Down => {
                if let Some(selected) = list.selected_row() {
                    let next_idx = selected.index() + 1;
                    if let Some(next_row) = list.row_at_index(next_idx) {
                        list.select_row(Some(&next_row));
                        next_row.grab_focus();
                    } else {
                        selected.grab_focus();
                    }
                } else if let Some(first_row) = list.row_at_index(0) {
                    list.select_row(Some(&first_row));
                    first_row.grab_focus();
                }
                glib::Propagation::Stop
            }
            gdk::Key::Up => {
                if let Some(selected) = list.selected_row() {
                    let prev_idx = selected.index() - 1;
                    if prev_idx >= 0 {
                        if let Some(prev_row) = list.row_at_index(prev_idx) {
                            list.select_row(Some(&prev_row));
                            prev_row.grab_focus();
                        }
                    } else {
                        search.grab_focus();
                    }
                } else {
                    search.grab_focus();
                }
                glib::Propagation::Stop
            }
            gdk::Key::Return | gdk::Key::KP_Enter => {
                Controller::activate_current_model_selection(&controller);
                glib::Propagation::Stop
            }
            gdk::Key::Escape => {
                popover.popdown();
                composer.grab_focus();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    controller
        .borrow()
        .widgets
        .model_search
        .add_controller(search_keys);

    let weak = Rc::downgrade(controller);
    let list_keys = gtk::EventControllerKey::new();
    list_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    list_keys.connect_key_pressed(move |_, key, _, _| {
        let Some(controller) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let (list, popover, composer, search) = {
            let this = controller.borrow();
            (
                this.widgets.model_list.clone(),
                this.widgets.model_popover.clone(),
                this.widgets.composer.clone(),
                this.widgets.model_search.clone(),
            )
        };
        match key {
            gdk::Key::Return | gdk::Key::KP_Enter => {
                Controller::activate_current_model_selection(&controller);
                glib::Propagation::Stop
            }
            gdk::Key::Escape => {
                popover.popdown();
                composer.grab_focus();
                glib::Propagation::Stop
            }
            gdk::Key::Up => {
                if let Some(selected) = list.selected_row() {
                    if selected.index() <= 0 {
                        search.grab_focus();
                        return glib::Propagation::Stop;
                    }
                }
                glib::Propagation::Proceed
            }
            _ => glib::Propagation::Proceed,
        }
    });
    controller
        .borrow()
        .widgets
        .model_list
        .add_controller(list_keys);

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .model_list
        .connect_row_activated(move |_, row| {
            if let Some(controller) = weak.upgrade() {
                let row_idx = row.index() as usize;
                let model_idx = controller
                    .borrow()
                    .widgets
                    .model_filtered_indices
                    .borrow()
                    .get(row_idx)
                    .copied();
                if let Some(model_idx) = model_idx {
                    Controller::select_model(&controller, model_idx);
                }
            }
        });

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .variant_button
        .connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::show_variant_picker(&controller);
            }
        });

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .variant_search
        .connect_search_changed(move |search| {
            if let Some(controller) = weak.upgrade() {
                Controller::filter_variant_list(&controller, search.text().as_str());
            }
        });

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .variant_search
        .connect_activate(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::activate_current_variant_selection(&controller);
            }
        });

    let weak = Rc::downgrade(controller);
    let variant_search_keys = gtk::EventControllerKey::new();
    variant_search_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    variant_search_keys.connect_key_pressed(move |_, key, _, _| {
        let Some(controller) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let (list, popover, composer, search) = {
            let this = controller.borrow();
            (
                this.widgets.variant_list.clone(),
                this.widgets.variant_popover.clone(),
                this.widgets.composer.clone(),
                this.widgets.variant_search.clone(),
            )
        };
        match key {
            gdk::Key::Down => {
                if let Some(selected) = list.selected_row() {
                    let next_idx = selected.index() + 1;
                    if let Some(next_row) = list.row_at_index(next_idx) {
                        list.select_row(Some(&next_row));
                        next_row.grab_focus();
                    } else {
                        selected.grab_focus();
                    }
                } else if let Some(first_row) = list.row_at_index(0) {
                    list.select_row(Some(&first_row));
                    first_row.grab_focus();
                }
                glib::Propagation::Stop
            }
            gdk::Key::Up => {
                if let Some(selected) = list.selected_row() {
                    let prev_idx = selected.index() - 1;
                    if prev_idx >= 0 {
                        if let Some(prev_row) = list.row_at_index(prev_idx) {
                            list.select_row(Some(&prev_row));
                            prev_row.grab_focus();
                        }
                    } else {
                        search.grab_focus();
                    }
                } else {
                    search.grab_focus();
                }
                glib::Propagation::Stop
            }
            gdk::Key::Return | gdk::Key::KP_Enter => {
                Controller::activate_current_variant_selection(&controller);
                glib::Propagation::Stop
            }
            gdk::Key::Escape => {
                popover.popdown();
                composer.grab_focus();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    controller
        .borrow()
        .widgets
        .variant_search
        .add_controller(variant_search_keys);

    let weak = Rc::downgrade(controller);
    let variant_list_keys = gtk::EventControllerKey::new();
    variant_list_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    variant_list_keys.connect_key_pressed(move |_, key, _, _| {
        let Some(controller) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let (list, popover, composer, search) = {
            let this = controller.borrow();
            (
                this.widgets.variant_list.clone(),
                this.widgets.variant_popover.clone(),
                this.widgets.composer.clone(),
                this.widgets.variant_search.clone(),
            )
        };
        match key {
            gdk::Key::Return | gdk::Key::KP_Enter => {
                Controller::activate_current_variant_selection(&controller);
                glib::Propagation::Stop
            }
            gdk::Key::Escape => {
                popover.popdown();
                composer.grab_focus();
                glib::Propagation::Stop
            }
            gdk::Key::Up => {
                if let Some(selected) = list.selected_row() {
                    if selected.index() <= 0 {
                        search.grab_focus();
                        return glib::Propagation::Stop;
                    }
                }
                glib::Propagation::Proceed
            }
            _ => glib::Propagation::Proceed,
        }
    });
    controller
        .borrow()
        .widgets
        .variant_list
        .add_controller(variant_list_keys);

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .variant_list
        .connect_row_activated(move |_, row| {
            if let Some(controller) = weak.upgrade() {
                let row_idx = row.index() as usize;
                let variant_idx = controller
                    .borrow()
                    .widgets
                    .variant_filtered_indices
                    .borrow()
                    .get(row_idx)
                    .copied();
                if let Some(variant_idx) = variant_idx {
                    Controller::select_variant(&controller, variant_idx);
                }
            }
        });

    let shortcuts = gtk::EventControllerKey::new();
    shortcuts.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(controller);
    shortcuts.connect_key_pressed(move |_, key, _, modifiers| {
        let Some(controller) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if key == gdk::Key::F2 && modifiers.is_empty() {
            Controller::rename_active_session(&controller);
            return glib::Propagation::Stop;
        }
        if key == gdk::Key::Escape && modifiers.is_empty() {
            let mut this = controller.borrow_mut();
            if this.widgets.new_session_overlay.is_visible() {
                this.close_new_session_overlay();
                return glib::Propagation::Stop;
            }
            if this.widgets.app_modal_overlay.is_visible() {
                this.close_app_modal();
                return glib::Propagation::Stop;
            }
            if this.widgets.composer_stack.visible_child_name().as_deref() == Some("prompt") {
                return glib::Propagation::Stop;
            }
            drop(this);
            Controller::acknowledge_active_unread(&controller);
            return glib::Propagation::Stop;
        }
        let alt = modifiers.contains(gdk::ModifierType::ALT_MASK)
            && !modifiers.contains(gdk::ModifierType::CONTROL_MASK);
        if is_alt_key(key) || alt {
            Controller::set_tab_shortcut_hint(&controller, true);
        }
        if alt {
            if let Some(index) = tab_index_from_key(key) {
                Controller::select_tab_number(&controller, index);
                return glib::Propagation::Stop;
            }
            return glib::Propagation::Proceed;
        }
        if !modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
            return glib::Propagation::Proceed;
        }
        match key {
            gdk::Key::b | gdk::Key::B => controller.borrow().toggle_sidebar(),
            gdk::Key::m | gdk::Key::M => {
                Controller::show_model_picker(&controller);
            }
            gdk::Key::slash | gdk::Key::KP_Divide => {
                Controller::show_variant_picker(&controller);
            }
            gdk::Key::g | gdk::Key::G => {
                controller.borrow().widgets.composer.grab_focus();
                return glib::Propagation::Stop;
            }
            gdk::Key::comma => Controller::show_settings(&controller, None),
            gdk::Key::p => Controller::show_session_picker(&controller),
            gdk::Key::t => Controller::show_new_session(&controller),
            gdk::Key::w => Controller::close_active(&controller),
            gdk::Key::u => Controller::pick_attachments(&controller),
            gdk::Key::Tab => Controller::cycle_tab(
                &controller,
                if modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
                    -1
                } else {
                    1
                },
            ),
            gdk::Key::ISO_Left_Tab => Controller::cycle_tab(&controller, -1),
            _ => {
                if let Some(index) = tab_index_from_key(key) {
                    Controller::select_tab_number(&controller, index);
                } else {
                    return glib::Propagation::Proceed;
                }
            }
        }
        glib::Propagation::Stop
    });
    let weak = Rc::downgrade(controller);
    shortcuts.connect_key_released(move |_, key, _, _| {
        if !is_alt_key(key) {
            return;
        }
        if let Some(controller) = weak.upgrade() {
            Controller::set_tab_shortcut_hint(&controller, false);
        }
    });
    controller.borrow().widgets.window.add_controller(shortcuts);

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .window
        .connect_notify_local(Some("is-active"), move |window, _| {
            if window.is_active() {
                return;
            }
            if let Some(controller) = weak.upgrade() {
                Controller::set_tab_shortcut_hint(&controller, false);
            }
        });
}

impl Controller {
    fn handle_ui_event(controller: &Rc<RefCell<Self>>, event: UiEvent) {
        match event {
            UiEvent::Connection { connected, error } => {
                let (api, commands) = {
                    let mut this = controller.borrow_mut();
                    this.event_connected = connected;
                    this.widgets.status.remove_css_class("error");
                    let mut commands = Vec::new();
                    if connected {
                        this.widgets.status.set_label("Connected");
                        if this.connected_once {
                            if this.bootstrap_pending {
                                this.bootstrap_reload_pending = true;
                            } else {
                                this.begin_bootstrap();
                                commands.push(Command::Bootstrap);
                            }
                            let loaded_sessions: Vec<_> = this
                                .state
                                .tabs
                                .iter()
                                .filter(|id| {
                                    this.state
                                        .conversations
                                        .get(*id)
                                        .is_some_and(|conversation| conversation.loaded)
                                })
                                .filter_map(|id| this.session(id).cloned())
                                .collect();
                            for session in loaded_sessions {
                                if !this.state.loading_messages.insert(session.id.clone()) {
                                    this.message_reload_pending.insert(session.id);
                                    continue;
                                }
                                this.replacing_messages.insert(session.id.clone());
                                commands.push(Command::LoadMessages {
                                    session_id: session.id,
                                    directory: session.directory,
                                    before: None,
                                });
                            }
                            let model_directories: HashSet<_> = this
                                .state
                                .tabs
                                .iter()
                                .filter_map(|id| this.session(id))
                                .map(|session| session.directory.clone())
                                .collect();
                            for directory in model_directories {
                                if this.state.loading_models.insert(directory.clone()) {
                                    commands.push(Command::LoadModels { directory });
                                } else {
                                    this.models_reload_pending.insert(directory);
                                }
                            }
                        }
                        this.connected_once = true;
                    } else {
                        let message = error
                            .as_deref()
                            .unwrap_or("Disconnected; reconnecting in the background");
                        this.widgets.status.set_label(message);
                        this.widgets
                            .status
                            .set_tooltip_text(this.credential_warning.as_deref().or(Some(message)));
                        this.widgets.status.add_css_class("error");
                    }
                    (this.api.clone(), commands)
                };
                for command in commands {
                    api.send(command);
                }
                if connected {
                    Self::refresh_all(controller);
                }
            }
            UiEvent::Bootstrap(result) => match result {
                Ok(bootstrap) => Self::apply_bootstrap(controller, bootstrap),
                Err(error) => {
                    let retry_immediately = {
                        let mut this = controller.borrow_mut();
                        this.bootstrap_pending = false;
                        this.show_error(&error);
                        if this.bootstrap_reload_pending {
                            this.bootstrap_reload_pending = false;
                            this.begin_bootstrap();
                            Some(this.api.clone())
                        } else {
                            None
                        }
                    };
                    if let Some(api) = retry_immediately {
                        api.send(Command::Bootstrap);
                    } else {
                        Self::schedule_bootstrap_retry(controller);
                    }
                }
            },
            UiEvent::MessagesLoaded {
                session_id,
                before,
                result,
            } => Self::apply_messages(controller, session_id, before, result),
            UiEvent::ModelsLoaded { directory, result } => {
                let mut this = controller.borrow_mut();
                this.state.loading_models.remove(&directory);
                if this.models_reload_pending.remove(&directory) {
                    this.state.loading_models.insert(directory.clone());
                    let api = this.api.clone();
                    drop(this);
                    api.send(Command::LoadModels { directory });
                    return;
                }
                match result {
                    Ok(catalog) => {
                        this.model_load_errors.remove(&directory);
                        this.state.catalogs.insert(directory.clone(), catalog);
                        if this.active_directory().as_deref() == Some(directory.as_str()) {
                            this.refresh_model_control();
                        }
                    }
                    Err(error) => {
                        this.model_load_errors
                            .insert(directory.clone(), error.clone());
                        this.show_error(&error);
                        if this.active_directory().as_deref() == Some(directory.as_str()) {
                            this.refresh_model_control();
                        }
                    }
                }
            }
            UiEvent::SessionCreated { request_id, result } => match result {
                Ok(session) => {
                    let mut this = controller.borrow_mut();
                    this.upsert_session(session.clone());
                    if this.pending_session_request == Some(request_id) {
                        this.pending_session_request = None;
                        this.close_new_session_overlay();
                    }
                    drop(this);
                    Self::open_tab(controller, &session.id);
                }
                Err(error) => {
                    let mut this = controller.borrow_mut();
                    if this.pending_session_request == Some(request_id) {
                        this.pending_session_request = None;
                        this.widgets.new_session_overlay.set_sensitive(true);
                    }
                    this.show_error(&error);
                }
            },
            UiEvent::SessionRenamed {
                request_id,
                session_id,
                result,
            } => match result {
                Ok(session) => {
                    let mut this = controller.borrow_mut();
                    if this.session(&session.id).is_some() {
                        this.upsert_session(session);
                    }
                    this.persist_state();
                    let weak = this.self_weak.clone();
                    this.refresh_tabs(&weak);
                    if this.pending_rename_request == Some(request_id) {
                        this.pending_rename_request = None;
                        this.close_app_modal();
                    }
                }
                Err(error) => {
                    let mut this = controller.borrow_mut();
                    if this.pending_rename_request == Some(request_id) {
                        this.pending_rename_request = None;
                        this.widgets.app_modal_card.set_sensitive(true);
                    }
                    this.show_error(&format!("Could not rename session {session_id}: {error}"));
                }
            },
            UiEvent::PromptAccepted {
                request_id,
                session_id,
                result,
            } => {
                let command = {
                    let mut this = controller.borrow_mut();
                    let current = this
                        .state
                        .pending_prompts
                        .get(&session_id)
                        .is_some_and(|pending| pending.request_id == request_id);
                    if !current {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            remove_pending_clipboard_attachments(
                                this.state.pending_prompts.remove(&session_id),
                            );
                            this.state.abort_requested.remove(&session_id).then(|| {
                                this.session(&session_id).map(|session| Command::Abort {
                                    session_id: session_id.clone(),
                                    directory: session.directory.clone(),
                                })
                            })
                        }
                        Err(error) => {
                            let status_changed =
                                this.update_session_status(&session_id, RunStatus::Idle);
                            this.state.server_busy.remove(&session_id);
                            this.state.abort_requested.remove(&session_id);
                            let pending = this
                                .state
                                .pending_prompts
                                .remove(&session_id)
                                .expect("matching pending prompt")
                                .draft;
                            this.state.optimistic_prompts.remove(&session_id);
                            let draft = this.state.drafts.entry(session_id.clone()).or_default();
                            if draft.text.trim().is_empty() {
                                draft.text = pending.text;
                            } else if !pending.text.trim().is_empty() {
                                draft.text = format!("{}\n\n{}", pending.text, draft.text);
                            }
                            for attachment in pending.attachments.into_iter().rev() {
                                if !draft.attachments.contains(&attachment) {
                                    draft.attachments.insert(0, attachment);
                                }
                            }
                            this.show_error(&error);
                            if this.state.active.as_deref() == Some(session_id.as_str()) {
                                this.refresh_composer();
                            }
                            if status_changed {
                                let weak = this.self_weak.clone();
                                this.refresh_tabs(&weak);
                            }
                            None
                        }
                    }
                    .flatten()
                };
                if let Some(command) = command {
                    controller.borrow().api.send(command);
                }
                let mut this = controller.borrow_mut();
                if this.state.active.as_deref() == Some(session_id.as_str()) {
                    this.refresh_transcript(TranscriptUpdate::Content);
                }
                this.refresh_send_button();
            }
            UiEvent::Aborted { session_id, result } => {
                let mut this = controller.borrow_mut();
                match result {
                    Ok(()) => {
                        this.state.server_busy.remove(&session_id);
                        this.state.abort_requested.remove(&session_id);
                        let status_changed =
                            this.update_session_status(&session_id, RunStatus::Idle);
                        if status_changed {
                            let weak = this.self_weak.clone();
                            this.refresh_tabs(&weak);
                        }
                        if this.state.active.as_deref() == Some(session_id.as_str()) {
                            this.refresh_transcript(TranscriptUpdate::Content);
                        }
                        this.refresh_send_button();
                    }
                    Err(error) => this.show_error(&error),
                }
            }
            UiEvent::ActionFinished { request_id, result } => {
                let mut this = controller.borrow_mut();
                this.pending_actions.remove(&request_id);
                match result {
                    Ok(()) => this.remove_composer_prompt(&request_id),
                    Err(error) => {
                        if let Some(dialog) = this.dialogs.get(&request_id) {
                            dialog.set_sensitive(true);
                        }
                        this.show_error(&error);
                    }
                }
            }
            UiEvent::ServerEvent(event) => Self::enqueue_server_event(controller, event),
        }
    }

    fn begin_bootstrap(&mut self) {
        self.bootstrap_pending = true;
        self.bootstrap_retry_token += 1;
        self.bootstrap_dialogs_at_start = self.dialogs.keys().cloned().collect();
    }

    fn schedule_bootstrap_retry(controller: &Rc<RefCell<Self>>) {
        let (token, delay) = {
            let mut this = controller.borrow_mut();
            this.bootstrap_retry_token += 1;
            let token = this.bootstrap_retry_token;
            let delay = this.bootstrap_retry_delay;
            this.bootstrap_retry_delay = (delay * 2).min(BOOTSTRAP_RETRY_MAX);
            (token, delay)
        };
        let weak = Rc::downgrade(controller);
        glib::timeout_add_local_once(delay, move || {
            let Some(controller) = weak.upgrade() else {
                return;
            };
            let api = {
                let mut this = controller.borrow_mut();
                if this.bootstrap_retry_token != token || this.bootstrap_pending {
                    return;
                }
                this.begin_bootstrap();
                this.api.clone()
            };
            api.send(Command::Bootstrap);
        });
    }

    fn apply_bootstrap(controller: &Rc<RefCell<Self>>, bootstrap: Bootstrap) {
        let mut pending = bootstrap.pending;
        let pending_complete = bootstrap.pending_complete;
        let statuses_complete = bootstrap.statuses_complete;
        let retry_needed = bootstrap.retry_needed;
        let warnings = bootstrap.warnings;
        let mut api_commands = Vec::new();
        let mut retry_immediately = None;
        {
            let mut this = controller.borrow_mut();
            this.bootstrap_pending = false;
            let partial_refresh = !warnings.is_empty();
            let persistence_failed = this.persistence_error.is_some();
            let credentials_failed = this.credential_warning.is_some();
            let mut warning_text = warnings;
            if let Some(error) = &this.persistence_error {
                warning_text.push(error.clone());
            }
            if let Some(warning) = &this.persistence_warning {
                warning_text.push(warning.clone());
            }
            if let Some(warning) = &this.credential_warning {
                warning_text.push(warning.clone());
            }
            if this.event_connected && !warning_text.is_empty() {
                this.widgets.status.set_label(&format!(
                    "Connected · {} · {}",
                    bootstrap.version,
                    if partial_refresh {
                        "Partial refresh"
                    } else if persistence_failed {
                        "State not saved"
                    } else if credentials_failed {
                        "Credentials unavailable"
                    } else {
                        "State recovered"
                    }
                ));
                this.widgets.status.add_css_class("error");
                this.widgets
                    .status
                    .set_tooltip_text(Some(&warning_text.join("\n")));
            } else if this.event_connected {
                this.widgets
                    .status
                    .set_label(&format!("Connected · {}", bootstrap.version));
                this.widgets.status.remove_css_class("error");
                this.widgets.status.set_tooltip_text(None);
            }
            let mut sessions = bootstrap.sessions;
            let mut removals = std::mem::take(&mut this.session_removals_during_bootstrap);
            for (id, event) in std::mem::take(&mut this.session_events_during_bootstrap) {
                if let Some(event) = event {
                    if let Some(session) = sessions.iter_mut().find(|session| session.id == id) {
                        if event.time.updated >= session.time.updated {
                            *session = event;
                        }
                    } else {
                        sessions.push(event);
                    }
                } else {
                    let removed_at = removals.remove(&id).unwrap_or(u64::MAX);
                    sessions
                        .retain(|session| session.id != id || session.time.updated > removed_at);
                }
            }
            if bootstrap.sessions_complete {
                this.state.sessions = sessions;
            } else {
                for session in sessions {
                    this.upsert_session(session);
                }
            }
            this.state
                .sessions
                .sort_by_key(|session| std::cmp::Reverse(session.time.updated));
            this.state.projects = bootstrap.projects;
            let mut statuses = bootstrap.statuses;
            statuses.extend(std::mem::take(&mut this.status_events_during_bootstrap));
            if statuses_complete {
                this.state.statuses = statuses.clone();
                this.state.server_busy.clear();
            } else {
                this.state.statuses.extend(statuses.clone());
            }
            for (session_id, status) in statuses {
                match status {
                    RunStatus::Busy | RunStatus::Retry { .. } => {
                        this.state.server_busy.insert(session_id.clone());
                        remove_pending_clipboard_attachments(
                            this.state.pending_prompts.remove(&session_id),
                        );
                        if this.state.abort_requested.remove(&session_id) {
                            if let Some(session) = this.session(&session_id) {
                                api_commands.push(Command::Abort {
                                    session_id: session_id.clone(),
                                    directory: session.directory.clone(),
                                });
                            }
                        }
                    }
                    RunStatus::Idle => {
                        this.state.server_busy.remove(&session_id);
                        remove_pending_clipboard_attachments(
                            this.state.pending_prompts.remove(&session_id),
                        );
                        this.state.abort_requested.remove(&session_id);
                    }
                }
            }
            this.apply_missed_idle(statuses_complete);
            let resolved = std::mem::take(&mut this.resolved_requests_during_bootstrap);
            pending.retain(|event| {
                event_data(&event.payload)
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|id| !resolved.contains(id))
            });
            if pending_complete {
                let pending_ids: HashSet<_> = pending
                    .iter()
                    .filter_map(|event| {
                        event_data(&event.payload)
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .collect();
                let resolved_dialogs: Vec<_> = this
                    .bootstrap_dialogs_at_start
                    .iter()
                    .filter(|id| !pending_ids.contains(*id))
                    .cloned()
                    .collect();
                for id in resolved_dialogs {
                    this.remove_composer_prompt(&id);
                }
            }
            let known: HashSet<_> = this
                .state
                .sessions
                .iter()
                .map(|session| session.id.clone())
                .collect();
            if bootstrap.sessions_complete {
                let stale_unread: Vec<_> = this
                    .state
                    .unread
                    .iter()
                    .filter(|id| !known.contains(*id))
                    .cloned()
                    .collect();
                for id in stale_unread {
                    this.clear_session_unread(&id);
                }
                this.state.tabs.retain(|id| known.contains(id));
                this.state.conversations.retain(|id, _| known.contains(id));
                this.state.selections.retain(|id, _| known.contains(id));
                this.state.drafts.retain(|id, _| known.contains(id));
                this.state
                    .pending_prompts
                    .retain(|id, _| known.contains(id));
                this.state
                    .optimistic_prompts
                    .retain(|id, _| known.contains(id));
                this.state.statuses.retain(|id, _| known.contains(id));
                this.state.server_busy.retain(|id| known.contains(id));
                this.state.abort_requested.retain(|id| known.contains(id));
                this.state.loading_messages.retain(|id| known.contains(id));
                this.replacing_messages.retain(|id| known.contains(id));
                this.message_reload_pending.retain(|id| known.contains(id));
                this.message_load_errors.retain(|id, _| known.contains(id));
                this.message_events_during_load
                    .retain(|id, _| known.contains(id));
            }
            if this
                .state
                .active
                .as_ref()
                .is_some_and(|id| !this.state.tabs.contains(id))
            {
                this.state.active = None;
            }
            if this.state.active.is_none() {
                this.state.active = this.state.tabs.last().cloned();
            }
            if !this.had_server_state && this.state.tabs.is_empty() {
                if let Some(id) = this
                    .state
                    .sessions
                    .first()
                    .map(|session| session.id.clone())
                {
                    this.state.tabs.push(id.clone());
                    this.state.active = Some(id);
                }
            }
            if bootstrap.sessions_complete || !this.state.sessions.is_empty() {
                this.had_server_state = true;
            }
            if !retry_needed {
                this.bootstrap_retry_delay = BOOTSTRAP_RETRY_MIN;
                this.bootstrap_retry_token += 1;
            }
            if this.bootstrap_reload_pending {
                this.bootstrap_reload_pending = false;
                this.begin_bootstrap();
                retry_immediately = Some(this.api.clone());
            }
            this.persist_state();
        }
        for command in api_commands {
            controller.borrow().api.send(command);
        }
        Self::refresh_all(controller);
        let active = controller.borrow().state.active.clone();
        if let Some(active) = active {
            Self::activate_tab(controller, &active);
        }
        Self::enqueue_bootstrap_events(controller, pending);
        if let Some(api) = retry_immediately {
            api.send(Command::Bootstrap);
        } else if retry_needed {
            Self::schedule_bootstrap_retry(controller);
        }
    }

    fn apply_messages(
        controller: &Rc<RefCell<Self>>,
        session_id: String,
        before: Option<String>,
        result: Result<MessagePage, String>,
    ) {
        let mut this = controller.borrow_mut();
        this.state.loading_messages.remove(&session_id);
        this.replacing_messages.remove(&session_id);
        let pending_events = this
            .message_events_during_load
            .remove(&session_id)
            .unwrap_or_default();
        if !this.state.tabs.contains(&session_id) {
            this.message_reload_pending.remove(&session_id);
            return;
        }
        match result {
            Ok(page) => {
                this.message_load_errors.remove(&session_id);
                let conversation = this
                    .state
                    .conversations
                    .entry(session_id.clone())
                    .or_default();
                if before.is_some() {
                    conversation.prepend_from_api(&page.messages, page.next_cursor);
                } else {
                    conversation.replace_from_api(&page.messages, page.next_cursor);
                }
                for event in pending_events {
                    conversation.apply_event(&event);
                }
                if this.state.active.as_deref() == Some(session_id.as_str()) {
                    this.refresh_transcript(if before.is_some() {
                        TranscriptUpdate::Prepend
                    } else {
                        TranscriptUpdate::Content
                    });
                    this.refresh_context_usage();
                }
            }
            Err(error) => {
                if before.is_none() {
                    this.message_load_errors
                        .insert(session_id.clone(), error.clone());
                }
                this.show_error(&error);
                if this.state.active.as_deref() == Some(session_id.as_str()) {
                    this.refresh_transcript(TranscriptUpdate::Content);
                    this.refresh_context_usage();
                }
            }
        }
        let reload = this
            .message_reload_pending
            .remove(&session_id)
            .then(|| this.session(&session_id).cloned())
            .flatten();
        if let Some(session) = reload {
            this.state.loading_messages.insert(session.id.clone());
            this.replacing_messages.insert(session.id.clone());
            if this.state.active.as_deref() == Some(session.id.as_str()) {
                this.refresh_transcript(TranscriptUpdate::Content);
            }
            let api = this.api.clone();
            drop(this);
            api.send(Command::LoadMessages {
                session_id: session.id,
                directory: session.directory,
                before: None,
            });
        }
    }

    fn enqueue_server_event(controller: &Rc<RefCell<Self>>, event: ServerEnvelope) {
        let mut this = controller.borrow_mut();
        this.pending_events.push(event);
        if this.event_flush_scheduled {
            return;
        }
        this.event_flush_scheduled = true;
        let weak = Rc::downgrade(controller);
        glib::timeout_add_local_once(STREAM_FRAME, move || {
            if let Some(controller) = weak.upgrade() {
                Self::flush_server_events(&controller);
            }
        });
    }

    fn enqueue_bootstrap_events(controller: &Rc<RefCell<Self>>, mut events: Vec<ServerEnvelope>) {
        if events.is_empty() {
            return;
        }
        let mut this = controller.borrow_mut();
        events.append(&mut this.pending_events);
        this.pending_events = events;
        if this.event_flush_scheduled {
            return;
        }
        this.event_flush_scheduled = true;
        let weak = Rc::downgrade(controller);
        glib::timeout_add_local_once(STREAM_FRAME, move || {
            if let Some(controller) = weak.upgrade() {
                Self::flush_server_events(&controller);
            }
        });
    }

    fn flush_server_events(controller: &Rc<RefCell<Self>>) {
        let mut permission_events = Vec::new();
        let mut question_events = Vec::new();
        let mut resolved_requests = HashSet::new();
        let mut api_commands = Vec::new();
        let mut this = controller.borrow_mut();
        this.event_flush_scheduled = false;
        let events = std::mem::take(&mut this.pending_events);
        let active = this.state.active.clone();
        let mut persist_unread = false;
        let mut transcript_changed = false;
        let mut tabs_changed = false;
        let mut tab_status_changed = false;

        for envelope in events {
            let payload = envelope.payload;
            if let Some(session) = event_session(&payload) {
                let open = this.state.tabs.contains(&session.id);
                if this.bootstrap_pending {
                    if session.time.archived.is_some() {
                        this.session_removals_during_bootstrap
                            .insert(session.id.clone(), session.time.updated);
                    } else {
                        this.session_removals_during_bootstrap.remove(&session.id);
                    }
                    this.session_events_during_bootstrap.insert(
                        session.id.clone(),
                        (session.time.archived.is_none() && session.parent_id.is_none())
                            .then(|| session.clone()),
                    );
                }
                this.upsert_session(session);
                tabs_changed |= open;
            }
            if let Some(id) = deleted_session_id(&payload) {
                let open = this.state.tabs.iter().any(|tab| tab == id);
                if this.bootstrap_pending {
                    this.session_events_during_bootstrap
                        .insert(id.to_owned(), None);
                    this.session_removals_during_bootstrap
                        .insert(id.to_owned(), u64::MAX);
                }
                this.remove_session(id);
                tabs_changed |= open;
            }
            if let Some((session_id, status)) = event_run_status(&payload) {
                if active.as_deref() == Some(session_id.as_str()) {
                    transcript_changed = true;
                }
                if this.bootstrap_pending {
                    this.status_events_during_bootstrap
                        .insert(session_id.clone(), status.clone());
                }
                match &status {
                    RunStatus::Busy | RunStatus::Retry { .. } => {
                        this.state.server_busy.insert(session_id.clone());
                        this.state.pending_prompts.remove(&session_id);
                        if this.state.abort_requested.remove(&session_id) {
                            if let Some(session) = this.session(&session_id) {
                                api_commands.push(Command::Abort {
                                    session_id: session_id.clone(),
                                    directory: session.directory.clone(),
                                });
                            }
                        }
                    }
                    RunStatus::Idle => {
                        this.state.server_busy.remove(&session_id);
                        this.state.abort_requested.remove(&session_id);
                        this.state.optimistic_prompts.remove(&session_id);
                        remove_pending_clipboard_attachments(
                            this.state.pending_prompts.remove(&session_id),
                        );
                    }
                }
                let previous = this.state.statuses.get(&session_id).cloned();
                let status_changed = this.update_session_status(&session_id, status.clone());
                let open = this.state.tabs.iter().any(|id| id == &session_id);
                if open && session_idle_marks_unread(previous.as_ref()) && status.is_idle() {
                    persist_unread |= this.state.unread.insert(session_id.clone());
                    tab_status_changed = true;
                    this.notify_session_idle(&session_id);
                }
                tab_status_changed |= status_changed && open;
            }
            if let Some(session_id) = event_session_id(&payload).map(str::to_owned) {
                if this.replacing_messages.contains(&session_id) {
                    this.message_events_during_load
                        .entry(session_id.clone())
                        .or_default()
                        .push(payload.clone());
                }
                if this.state.tabs.contains(&session_id)
                    || this.state.conversations.contains_key(&session_id)
                {
                    let conversation = this
                        .state
                        .conversations
                        .entry(session_id.clone())
                        .or_default();
                    let changed = conversation.apply_event(&payload);
                    if active.as_deref() == Some(session_id.as_str()) {
                        transcript_changed |= changed;
                    }
                }
            }
            match payload.get("type").and_then(serde_json::Value::as_str) {
                Some("permission.asked") | Some("permission.updated") => {
                    permission_events.push((envelope.directory, payload));
                }
                Some("question.asked") => question_events.push((envelope.directory, payload)),
                Some("permission.replied")
                | Some("question.replied")
                | Some("question.rejected") => {
                    let data = event_data(&payload);
                    if let Some(request_id) = data
                        .get("requestID")
                        .or_else(|| data.get("permissionID"))
                        .and_then(serde_json::Value::as_str)
                    {
                        resolved_requests.insert(request_id.to_owned());
                        if this.bootstrap_pending {
                            this.resolved_requests_during_bootstrap
                                .insert(request_id.to_owned());
                        }
                        this.remove_composer_prompt(request_id);
                    }
                }
                Some("server.instance.disposed") => {
                    if let Some(directory) = envelope.directory {
                        this.state.catalogs.remove(&directory);
                        this.model_load_errors.remove(&directory);
                        if this
                            .state
                            .tabs
                            .iter()
                            .filter_map(|id| this.session(id))
                            .any(|session| session.directory == directory)
                        {
                            if this.state.loading_models.insert(directory.clone()) {
                                this.api.send(Command::LoadModels { directory });
                            } else {
                                this.models_reload_pending.insert(directory);
                            }
                        }
                    }
                    if this.bootstrap_pending {
                        this.bootstrap_reload_pending = true;
                    } else {
                        this.begin_bootstrap();
                        this.api.send(Command::Bootstrap);
                    }
                }
                _ => {}
            }
        }
        permission_events.retain(|(_, payload)| {
            event_data(payload)
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|id| !resolved_requests.contains(id))
        });
        question_events.retain(|(_, payload)| {
            event_data(payload)
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|id| !resolved_requests.contains(id))
        });
        if tabs_changed {
            this.persist_state();
            drop(this);
            Self::refresh_all(controller);
            this = controller.borrow_mut();
        } else if tab_status_changed {
            if persist_unread {
                this.persist_state();
            }
            let weak = this.self_weak.clone();
            this.refresh_tabs(&weak);
        }
        if transcript_changed {
            this.refresh_transcript(TranscriptUpdate::Content);
            this.refresh_context_usage();
        }
        this.refresh_send_button();
        let activate_fallback = tabs_changed
            .then(|| this.state.active.clone())
            .flatten()
            .filter(|new_active| active.as_deref() != Some(new_active.as_str()));
        drop(this);

        if let Some(active) = activate_fallback {
            Self::activate_tab(controller, &active);
        }
        for command in api_commands {
            controller.borrow().api.send(command);
        }
        for (directory, payload) in permission_events {
            Self::show_permission(controller, directory, payload);
        }
        for (directory, payload) in question_events {
            Self::show_question(controller, directory, payload);
        }
    }

    fn refresh_all(controller: &Rc<RefCell<Self>>) {
        let weak = Rc::downgrade(controller);
        let mut this = controller.borrow_mut();
        this.refresh_tabs(&weak);
        this.refresh_composer();
        this.refresh_transcript(TranscriptUpdate::Content);
    }

    fn refresh_tabs(&mut self, weak: &Weak<RefCell<Self>>) {
        {
            let mut dnd = self.widgets.tab_dnd.borrow_mut();
            dnd.gen = dnd.gen.wrapping_add(1);
            dnd.index = None;
            dnd.slot = None;
        }
        self.widgets.tab_bar.remove_css_class("reordering");
        clear_box(&self.widgets.tab_bar);
        let mut previous_inactive = false;
        for (index, id) in self.state.tabs.clone().into_iter().enumerate() {
            let title = self
                .session(&id)
                .map(|session| session.title.clone())
                .unwrap_or_else(|| "Unknown session".to_owned());
            let tab = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            tab.add_css_class("session-tab");
            tab.set_widget_name(&id);
            let active = self.state.active.as_deref() == Some(id.as_str());
            let unread = self.state.unread.contains(&id);
            let busy = self.state.statuses.get(&id).is_some_and(|s| s.is_busy());
            if active {
                tab.add_css_class("active");
                previous_inactive = false;
            } else {
                tab.add_css_class("inactive");
                if previous_inactive {
                    tab.add_css_class("divided");
                }
                previous_inactive = true;
            }
            if unread {
                tab.add_css_class("unread");
            }
            if busy {
                tab.add_css_class("busy");
            }
            let status = if busy {
                let spinner = gtk::Spinner::new();
                spinner.set_size_request(14, 14);
                spinner.start();
                spinner.upcast::<gtk::Widget>()
            } else {
                gtk::Box::new(gtk::Orientation::Horizontal, 0).upcast::<gtk::Widget>()
            };
            status.add_css_class("session-tab-status");
            status.add_css_class(if busy {
                "busy"
            } else if unread {
                "unread"
            } else {
                "idle"
            });
            status.set_halign(gtk::Align::Center);
            status.set_valign(gtk::Align::Center);
            status.set_tooltip_text(Some(if busy {
                "Session is working"
            } else if unread {
                "Session has unread output"
            } else {
                "Session is idle"
            }));
            let show_index = self.tab_shortcut_hint && index < 9;
            let hint = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            hint.add_css_class("session-tab-hint");
            hint.set_halign(gtk::Align::Center);
            hint.set_valign(gtk::Align::Center);
            hint.set_size_request(14, 14);
            status.set_visible(!show_index);
            hint.append(&status);
            if index < 9 {
                let number = gtk::Label::new(Some(&(index + 1).to_string()));
                number.add_css_class("session-tab-index");
                number.set_halign(gtk::Align::Center);
                number.set_valign(gtk::Align::Center);
                number.set_visible(show_index);
                hint.append(&number);
            }
            let title_label = gtk::Label::new(Some(&title));
            title_label.set_xalign(0.0);
            title_label.set_hexpand(true);
            title_label.set_ellipsize(pango::EllipsizeMode::End);
            title_label.set_width_chars(8);
            title_label.set_max_width_chars(24);
            title_label.add_css_class("session-tab-title");
            let rename = icon_button(ICON_EDIT, TAB_ICON_PX);
            rename.set_tooltip_text(Some("Rename session (F2)"));
            rename.set_valign(gtk::Align::Center);
            rename.add_css_class("flat");
            rename.add_css_class("session-tab-action");
            let close = icon_button(ICON_CLOSE, TAB_ICON_PX);
            close.set_tooltip_text(Some("Close tab"));
            close.set_valign(gtk::Align::Center);
            close.add_css_class("flat");
            close.add_css_class("session-tab-action");
            close.add_css_class("session-tab-close");
            tab.set_tooltip_text(
                self.session(&id)
                    .map(|session| format!("{}\nOpen session", session.directory))
                    .as_deref(),
            );
            tab.append(&hint);
            tab.append(&title_label);
            tab.append(&rename);
            tab.append(&close);

            let weak_select = weak.clone();
            let select_id = id.clone();
            let rename_hit = rename.clone();
            let close_hit = close.clone();
            let click_tab = tab.clone();
            let click = gtk::GestureClick::new();
            click.set_button(1);
            click.connect_released(move |_, _, x, y| {
                if pointer_hits_widget(&rename_hit, &click_tab, x, y)
                    || pointer_hits_widget(&close_hit, &click_tab, x, y)
                {
                    return;
                }
                if let Some(controller) = weak_select.upgrade() {
                    Self::activate_tab(&controller, &select_id);
                }
            });
            tab.add_controller(click);
            let weak_rename = weak.clone();
            let rename_id = id.clone();
            rename.connect_clicked(move |_| {
                if let Some(controller) = weak_rename.upgrade() {
                    Self::show_rename_session(&controller, &rename_id);
                }
            });
            let weak_close = weak.clone();
            let close_id = id.clone();
            close.connect_clicked(move |_| {
                if let Some(controller) = weak_close.upgrade() {
                    Self::close_tab(&controller, &close_id);
                }
            });
            let middle = gtk::GestureClick::new();
            middle.set_button(2);
            let weak_middle = weak.clone();
            let middle_id = id.clone();
            middle.connect_released(move |_, _, _, _| {
                if let Some(controller) = weak_middle.upgrade() {
                    Self::close_tab(&controller, &middle_id);
                }
            });
            tab.add_controller(middle);

            let drag = gtk::DragSource::new();
            drag.set_actions(gdk::DragAction::MOVE);
            let drag_tab = tab.clone();
            let drag_rename = rename.clone();
            let drag_close = close.clone();
            let drag_id = id.clone();
            let weak_prepare = weak.clone();
            drag.connect_prepare(move |_, x, y| {
                if weak_prepare
                    .upgrade()
                    .is_some_and(|controller| controller.borrow().tab_shortcut_hint)
                    || pointer_hits_widget(&drag_rename, &drag_tab, x, y)
                    || pointer_hits_widget(&drag_close, &drag_tab, x, y)
                {
                    None
                } else {
                    Some(gdk::ContentProvider::for_value(&drag_id.to_value()))
                }
            });
            let drag_tab = tab.clone();
            let drag_title = title.clone();
            let drag_active = active;
            let dnd = self.widgets.tab_dnd.clone();
            let weak_begin = weak.clone();
            drag.connect_drag_begin(move |_, drag| {
                let icon = gtk::DragIcon::for_drag(drag);
                let preview = tab_drag_preview(&drag_title, drag_active);
                preview.set_size_request(drag_tab.width().max(180), drag_tab.height().max(36));
                icon.set_child(Some(&preview));
                let index = visible_tab_index(&drag_tab).unwrap_or(0);
                drag_tab.add_css_class("dragging");
                drag_tab.set_visible(false);
                if let Some(bar) = drag_tab
                    .parent()
                    .and_then(|parent| parent.downcast::<gtk::Box>().ok())
                {
                    place_drop_slot(&bar, index, &dnd, &weak_begin);
                }
            });
            let weak_end = weak.clone();
            drag.connect_drag_end(move |_, _, _| {
                if let Some(controller) = weak_end.upgrade() {
                    let mut this = controller.borrow_mut();
                    let restore = this.self_weak.clone();
                    this.refresh_tabs(&restore);
                }
            });
            tab.add_controller(drag);

            let drop_target = gtk::DropTarget::new(String::static_type(), gdk::DragAction::MOVE);
            let dnd = self.widgets.tab_dnd.clone();
            let weak_motion = weak.clone();
            drop_target.connect_motion(move |target, _, y| {
                if weak_motion
                    .upgrade()
                    .is_some_and(|controller| controller.borrow().tab_shortcut_hint)
                {
                    return gdk::DragAction::empty();
                }
                if let Some(widget) = target.widget() {
                    let after = y >= f64::from(widget.height()) / 2.0;
                    if let Some(bar) = widget
                        .parent()
                        .and_then(|parent| parent.downcast::<gtk::Box>().ok())
                    {
                        place_drop_slot(
                            &bar,
                            insert_index_for_target(&widget, after),
                            &dnd,
                            &weak_motion,
                        );
                    }
                }
                gdk::DragAction::MOVE
            });
            let weak_drop = weak.clone();
            drop_target.connect_drop(move |_, value, _, _| {
                let Ok(source_id) = value.get::<String>() else {
                    return false;
                };
                let Some(controller) = weak_drop.upgrade() else {
                    return false;
                };
                if controller.borrow().tab_shortcut_hint {
                    return false;
                }
                Self::commit_tab_order(&controller, &source_id)
            });
            tab.add_controller(drop_target);
            self.widgets.tab_bar.append(&tab);
        }
        self.refresh_session_header();
    }

    fn open_tab(controller: &Rc<RefCell<Self>>, id: &str) {
        {
            let mut this = controller.borrow_mut();
            if this.session(id).is_none() {
                this.show_error("That session is no longer available");
                return;
            }
            if !this.state.tabs.iter().any(|tab| tab == id) {
                this.state.tabs.push(id.to_owned());
            }
        }
        Self::activate_tab(controller, id);
    }

    fn activate_tab(controller: &Rc<RefCell<Self>>, id: &str) {
        let (load_messages, load_models) = {
            let mut this = controller.borrow_mut();
            let Some(session) = this.session(id).cloned() else {
                return;
            };
            let active_changed = this.state.active.as_deref() != Some(id);
            this.state.active = Some(id.to_owned());
            let load_messages = !this
                .state
                .conversations
                .get(id)
                .is_some_and(|conversation| conversation.loaded)
                && this.state.loading_messages.insert(id.to_owned());
            if load_messages {
                this.replacing_messages.insert(id.to_owned());
            }
            let load_models = !this.state.catalogs.contains_key(&session.directory)
                && this.state.loading_models.insert(session.directory.clone());
            this.pin_transcript_to_bottom();
            if active_changed {
                this.persist_state();
            }
            (
                load_messages.then_some(session.clone()),
                load_models.then_some(session),
            )
        };
        let weak = Rc::downgrade(controller);
        let mut this = controller.borrow_mut();
        this.refresh_tabs(&weak);
        this.refresh_composer();
        this.refresh_transcript(TranscriptUpdate::Activate);
        drop(this);
        let this = controller.borrow();
        if let Some(session) = load_messages {
            this.api.send(Command::LoadMessages {
                session_id: session.id,
                directory: session.directory,
                before: None,
            });
        }
        if let Some(session) = load_models {
            this.api.send(Command::LoadModels {
                directory: session.directory,
            });
        }
        if this.widgets.composer_stack.visible_child_name().as_deref() == Some("composer") {
            this.widgets.composer.grab_focus();
        }
    }

    fn commit_tab_order(controller: &Rc<RefCell<Self>>, source_id: &str) -> bool {
        let mut this = controller.borrow_mut();
        let mut ids = visible_tab_ids(&this.widgets.tab_bar);
        let index = this
            .widgets
            .tab_dnd
            .borrow()
            .index
            .unwrap_or(ids.len())
            .min(ids.len());
        if !ids.iter().any(|id| id == source_id) {
            ids.insert(index, source_id.to_owned());
        }
        if ids.is_empty() {
            return false;
        }
        if ids != this.state.tabs {
            this.state.tabs = ids;
            this.persist_state();
        }
        true
    }

    fn close_tab(controller: &Rc<RefCell<Self>>, id: &str) {
        {
            let mut this = controller.borrow_mut();
            let Some(index) = this.state.tabs.iter().position(|tab| tab == id) else {
                return;
            };
            this.state.tabs.remove(index);
            this.state.conversations.remove(id);
            this.state.loading_messages.remove(id);
            this.replacing_messages.remove(id);
            this.message_reload_pending.remove(id);
            this.message_load_errors.remove(id);
            this.message_events_during_load.remove(id);
            this.clear_session_unread(id);
            if this.state.active.as_deref() == Some(id) {
                this.state.active = this
                    .state
                    .tabs
                    .get(index)
                    .or_else(|| {
                        index
                            .checked_sub(1)
                            .and_then(|index| this.state.tabs.get(index))
                    })
                    .cloned();
            }
            this.persist_state();
        }
        Self::refresh_all(controller);
        let active = controller.borrow().state.active.clone();
        if let Some(active) = active {
            Self::activate_tab(controller, &active);
        }
    }

    fn close_active(controller: &Rc<RefCell<Self>>) {
        let active = controller.borrow().state.active.clone();
        if let Some(active) = active {
            Self::close_tab(controller, &active);
        }
    }

    fn cycle_tab(controller: &Rc<RefCell<Self>>, direction: isize) {
        let next = {
            let this = controller.borrow();
            if this.state.tabs.is_empty() {
                return;
            }
            let index = this
                .state
                .active
                .as_ref()
                .and_then(|id| this.state.tabs.iter().position(|tab| tab == id))
                .unwrap_or_default() as isize;
            let next = (index + direction).rem_euclid(this.state.tabs.len() as isize) as usize;
            this.state.tabs[next].clone()
        };
        Self::activate_tab(controller, &next);
    }

    fn select_tab_number(controller: &Rc<RefCell<Self>>, index: usize) {
        let id = controller.borrow().state.tabs.get(index).cloned();
        if let Some(id) = id {
            Self::activate_tab(controller, &id);
        }
    }

    fn set_tab_shortcut_hint(controller: &Rc<RefCell<Self>>, held: bool) {
        let mut this = controller.borrow_mut();
        if this.tab_shortcut_hint == held {
            return;
        }
        this.tab_shortcut_hint = held;
        let dragging = {
            let dnd = this.widgets.tab_dnd.borrow();
            dnd.index.is_some() || dnd.slot.is_some()
        };
        if held && dragging {
            let restore = this.self_weak.clone();
            this.refresh_tabs(&restore);
            return;
        }
        apply_tab_shortcut_hint(&this.widgets.tab_bar, held);
    }

    fn refresh_composer(&mut self) {
        self.controls_updating = true;
        let active = self.state.active.clone();
        let draft = active
            .as_ref()
            .and_then(|active| self.state.drafts.get(active))
            .cloned()
            .unwrap_or_default();
        let buffer = self.widgets.composer.buffer();
        if buffer_text(&buffer) != draft.text {
            buffer.set_text(&draft.text);
        }
        self.widgets.composer.set_editable(active.is_some());
        self.refresh_attachments();
        self.refresh_model_control();
        self.controls_updating = false;
        self.refresh_send_button();
        self.refresh_context_usage();
        self.refresh_composer_prompt();
    }

    fn refresh_composer_prompt(&mut self) {
        let active = self.state.active.as_deref();
        let shown_matches = self
            .shown_composer_prompt
            .as_ref()
            .is_some_and(|request_id| {
                self.composer_prompts
                    .iter()
                    .find(|prompt| &prompt.request_id == request_id)
                    .is_some_and(|prompt| {
                        prompt
                            .session_id
                            .as_deref()
                            .is_none_or(|session_id| Some(session_id) == active)
                    })
            });
        if !shown_matches {
            clear_box(&self.widgets.prompt_host);
            self.shown_composer_prompt = None;
            self.widgets
                .composer_stack
                .set_visible_child_name("composer");
        }
        if self.shown_composer_prompt.is_none() {
            if let Some(prompt) = self.composer_prompts.iter().find(|prompt| {
                prompt
                    .session_id
                    .as_deref()
                    .is_none_or(|session_id| Some(session_id) == active)
            }) {
                clear_box(&self.widgets.prompt_host);
                self.widgets.prompt_host.append(&prompt.widget);
                self.shown_composer_prompt = Some(prompt.request_id.clone());
                self.widgets.composer_stack.set_visible_child_name("prompt");
            }
        }
    }

    fn remove_composer_prompt(&mut self, request_id: &str) {
        self.dialogs.remove(request_id);
        self.pending_actions.remove(request_id);
        self.composer_prompts
            .retain(|prompt| prompt.request_id != request_id);
        if self.shown_composer_prompt.as_deref() == Some(request_id) {
            clear_box(&self.widgets.prompt_host);
            self.shown_composer_prompt = None;
            self.widgets
                .composer_stack
                .set_visible_child_name("composer");
        }
        self.refresh_composer_prompt();
    }

    fn refresh_model_control(&mut self) {
        self.controls_updating = true;
        self.widgets.model_button.set_tooltip_text(None);
        self.widgets.attach_button.set_sensitive(false);
        self.widgets
            .attach_button
            .set_tooltip_text(Some("Select a model that accepts attachments"));
        let Some(active) = self.state.active.clone() else {
            self.current_models.clear();
            self.widgets.model_button_label.set_text("No session");
            self.widgets.model_button.set_sensitive(false);
            self.refresh_variant_control();
            self.controls_updating = false;
            self.refresh_context_usage();
            return;
        };
        let Some(directory) = self
            .session(&active)
            .map(|session| session.directory.clone())
        else {
            self.controls_updating = false;
            self.refresh_context_usage();
            return;
        };
        let Some(catalog) = self.state.catalogs.get(&directory).cloned() else {
            self.current_models.clear();
            let loading = self.state.loading_models.contains(&directory);
            let error = self.model_load_errors.get(&directory);
            self.widgets.model_button_label.set_text(if loading {
                "Loading models..."
            } else if error.is_some() {
                "Could not load models"
            } else {
                "No models available"
            });
            self.widgets
                .model_button
                .set_tooltip_text(error.map(String::as_str));
            self.widgets.model_button.set_sensitive(false);
            self.refresh_variant_control();
            self.controls_updating = false;
            self.refresh_context_usage();
            return;
        };

        self.widgets.model_button.set_tooltip_text(None);
        self.current_models = catalog.models.clone();
        if self.current_models.is_empty() {
            self.state.selections.remove(&active);
            self.widgets
                .model_button_label
                .set_text("No models available");
            self.widgets.model_button.set_sensitive(false);
            self.refresh_variant_control();
            self.controls_updating = false;
            self.refresh_context_usage();
            return;
        }
        let selection = self.current_model_selection_for_active();
        if let Some(selection) = selection {
            let index = self
                .current_models
                .iter()
                .position(|model| {
                    model.provider_id == selection.provider_id
                        && model.model_id == selection.model_id
                })
                .unwrap_or_default();
            if let Some(model) = self.current_models.get(index) {
                self.widgets.model_button_label.set_text(&model.label);
            }
        }
        let model_ready = !self.state.loading_models.contains(&directory)
            && !self.model_load_errors.contains_key(&directory);
        if self.state.loading_models.contains(&directory) {
            self.widgets
                .model_button
                .set_tooltip_text(Some("Refreshing models..."));
        } else if let Some(error) = self.model_load_errors.get(&directory) {
            self.widgets.model_button.set_tooltip_text(Some(error));
        } else {
            self.widgets
                .model_button
                .set_tooltip_text(Some("Select model (Ctrl+M)"));
        }
        self.widgets.model_button.set_sensitive(model_ready);
        self.refresh_variant_control();
        self.refresh_attachment_control();
        self.controls_updating = false;
        self.refresh_context_usage();
    }

    fn current_model_selection_for_active(&self) -> Option<ModelSelection> {
        let active = self.state.active.as_deref()?;
        let session = self.session(active);
        let directory = session.map(|s| s.directory.as_str()).unwrap_or("");
        let catalog = self.state.catalogs.get(directory)?;
        let session_selection = session
            .and_then(Session::model_selection)
            .filter(|selection| catalog.find(selection).is_some());
        self.state
            .selections
            .get(active)
            .filter(|selection| catalog.find(selection).is_some())
            .cloned()
            .or(session_selection)
            .or_else(|| catalog.preferred.clone())
            .or_else(|| {
                self.current_models.first().map(|model| ModelSelection {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                    variant: None,
                })
            })
    }

    fn refresh_variant_control(&mut self) {
        let selection = self.current_model_selection_for_active();
        let model = selection.as_ref().and_then(|selection| {
            self.current_models.iter().find(|model| {
                model.provider_id == selection.provider_id && model.model_id == selection.model_id
            })
        });
        self.current_variants = std::iter::once(None)
            .chain(
                model
                    .into_iter()
                    .flat_map(|model| model.variants.iter().cloned().map(Some)),
            )
            .collect();
        let saved = selection.as_ref().map(|selection| &selection.variant);
        let (selected, clear_invalid) =
            resolved_variant_index(saved, &self.current_variants, model.is_some());
        if clear_invalid {
            if let Some(active) = self.state.active.as_ref() {
                if let Some(selection) = self.state.selections.get_mut(active) {
                    selection.variant = None;
                }
            }
            self.persist_state();
        }
        let label = self
            .current_variants
            .get(selected)
            .and_then(|v| v.as_deref())
            .unwrap_or("Default");
        self.widgets.variant_button_label.set_text(label);
        self.widgets
            .variant_button
            .set_sensitive(self.current_variants.len() > 1);
    }

    fn refresh_attachments(&mut self) {
        clear_box(&self.widgets.attachment_box);
        let Some(active) = self.state.active.clone() else {
            self.widgets.attachment_box.set_visible(false);
            return;
        };
        let attachments = self
            .state
            .drafts
            .get(&active)
            .map(|draft| draft.attachments.clone())
            .unwrap_or_default();
        self.widgets
            .attachment_box
            .set_visible(!attachments.is_empty());
        for path in attachments {
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("attachment");
            let button = gtk::Button::with_label(&format!("{filename}  ×"));
            button.add_css_class("attachment-chip");
            let draft_id = active.clone();
            let weak = self.self_weak.clone();
            button.connect_clicked(move |_| {
                let Some(controller) = weak.upgrade() else {
                    return;
                };
                let mut this = controller.borrow_mut();
                if let Some(draft) = this.state.drafts.get_mut(&draft_id) {
                    draft.attachments.retain(|attachment| attachment != &path);
                }
                remove_clipboard_attachment(&path);
                if this.state.active.as_deref() == Some(draft_id.as_str()) {
                    this.refresh_attachments();
                    this.refresh_send_button();
                }
            });
            self.widgets.attachment_box.append(&button);
        }
    }

    fn refresh_send_button(&mut self) {
        let Some(active) = self.state.active.as_ref() else {
            set_button_icon(&self.widgets.send_button, ICON_SEND, COMPOSER_ICON_PX);
            self.widgets
                .send_button
                .set_tooltip_text(Some("Send prompt"));
            self.widgets.send_button.set_sensitive(false);
            return;
        };
        let busy = self.state.statuses.get(active).is_some_and(|s| s.is_busy());
        set_button_icon(
            &self.widgets.send_button,
            if busy { ICON_STOP } else { ICON_SEND },
            COMPOSER_ICON_PX,
        );
        self.widgets.send_button.set_tooltip_text(Some(if busy {
            "Stop generation"
        } else {
            "Send prompt"
        }));
        let draft = self.state.drafts.get(active);
        let has_input = draft
            .is_some_and(|draft| !draft.text.trim().is_empty() || !draft.attachments.is_empty());
        let attachments_valid = draft.is_none_or(|draft| {
            draft.attachments.is_empty() || self.selected_model_supports_attachments()
        });
        let model_selection = self.current_model_selection_for_active();
        let model_ready = self.session(active).is_some_and(|session| {
            !self.state.loading_models.contains(&session.directory)
                && !self.model_load_errors.contains_key(&session.directory)
                && model_selection.as_ref().is_some_and(|selection| {
                    self.state
                        .catalogs
                        .get(&session.directory)
                        .is_some_and(|catalog| catalog.find(selection).is_some())
                })
        });
        self.widgets
            .send_button
            .set_sensitive(busy || (has_input && attachments_valid && model_ready));
    }

    fn selected_model_supports_attachments(&self) -> bool {
        let Some(selection) = self.current_model_selection_for_active() else {
            return false;
        };
        self.current_models.iter().any(|model| {
            model.provider_id == selection.provider_id
                && model.model_id == selection.model_id
                && model.supports_attachments
        })
    }

    fn selected_context_limit(&self) -> Option<u64> {
        let selection = self.current_model_selection_for_active()?;
        self.current_models
            .iter()
            .find(|model| {
                model.provider_id == selection.provider_id && model.model_id == selection.model_id
            })
            .and_then(|model| model.context_limit)
            .filter(|limit| *limit > 0)
    }

    fn refresh_context_usage(&self) {
        let Some(limit) = self.selected_context_limit() else {
            self.widgets.context_usage.set_text("");
            self.widgets.context_usage.set_tooltip_text(None);
            self.widgets.context_usage.set_visible(false);
            return;
        };
        let active = self.state.active.as_ref();
        let loading = active.is_some_and(|id| self.state.loading_messages.contains(id));
        if loading {
            self.widgets.context_usage.set_text("");
            self.widgets.context_usage.set_tooltip_text(None);
            self.widgets.context_usage.set_visible(false);
            return;
        }
        let used = active
            .and_then(|id| self.state.conversations.get(id))
            .and_then(Conversation::context_tokens)
            .unwrap_or(0);
        self.widgets
            .context_usage
            .set_text(&format_context_usage(used, limit));
        self.widgets
            .context_usage
            .set_tooltip_text(Some(&format!("{used} / {limit} tokens")));
        self.widgets.context_usage.set_visible(true);
    }

    fn toggle_sidebar(&self) {
        let visible = !self.widgets.sidebar.is_visible();
        self.widgets.sidebar.set_visible(visible);
        self.widgets
            .sidebar_toggle
            .set_tooltip_text(Some(if visible {
                "Hide sidebar (Ctrl+B)"
            } else {
                "Show sidebar (Ctrl+B)"
            }));
        self.refresh_session_header();
    }

    fn refresh_session_header(&self) {
        let visible = !self.widgets.sidebar.is_visible();
        self.widgets.session_header_bar.set_visible(visible);
        if visible {
            let active_id = self.state.active.as_deref();
            let active_title = active_id
                .and_then(|id| self.session(id))
                .map(|session| session.title.trim())
                .filter(|title| !title.is_empty())
                .unwrap_or("OpenCode");
            self.widgets.session_header_title.set_label(active_title);
            self.widgets
                .session_header_title
                .set_tooltip_text(Some(active_title));

            let busy = active_id.is_some_and(|id| {
                self.state
                    .statuses
                    .get(id)
                    .is_some_and(|status| status.is_busy())
            });
            let unread = active_id.is_some_and(|id| self.state.unread.contains(id));

            clear_box(&self.widgets.session_header_status);
            let status_widget = if busy {
                let spinner = gtk::Spinner::new();
                spinner.set_size_request(14, 14);
                spinner.start();
                spinner.upcast::<gtk::Widget>()
            } else {
                let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                dot.set_size_request(14, 14);
                dot.upcast::<gtk::Widget>()
            };
            status_widget.add_css_class("session-tab-status");
            status_widget.add_css_class(if busy {
                "busy"
            } else if unread {
                "unread"
            } else {
                "idle"
            });
            status_widget.set_halign(gtk::Align::Center);
            status_widget.set_valign(gtk::Align::Center);
            status_widget.set_tooltip_text(Some(if busy {
                "Session is working"
            } else if unread {
                "Session has unread output"
            } else {
                "Session is idle"
            }));
            self.widgets.session_header_status.append(&status_widget);
        }
    }

    fn refresh_attachment_control(&self) {
        let model_ready = self
            .state
            .active
            .as_ref()
            .and_then(|active| self.session(active))
            .is_some_and(|session| {
                !self.state.loading_models.contains(&session.directory)
                    && !self.model_load_errors.contains_key(&session.directory)
            });
        let supported = model_ready && self.selected_model_supports_attachments();
        self.widgets.attach_button.set_sensitive(supported);
        self.widgets
            .attach_button
            .set_tooltip_text(Some(if supported {
                "Attach files (Ctrl+U)"
            } else {
                "The selected model does not accept attachments"
            }));
    }

    fn pin_transcript_to_bottom(&mut self) {
        self.transcript_at_bottom = true;
        self.transcript_scroll_generation += 1;
        self.widgets.transcript_user_scrolling.set(false);
    }

    fn refresh_transcript(&mut self, update: TranscriptUpdate) {
        let active = self.state.active.clone();
        let mut rows = active
            .as_ref()
            .and_then(|active| self.state.conversations.get(active))
            .map(Conversation::transcript_rows)
            .unwrap_or_default();
        if let Some(session_id) = active.as_ref() {
            let optimistic = self.state.optimistic_prompts.get(session_id);
            let (next, superseded) = apply_optimistic_row(rows, optimistic);
            rows = next;
            if superseded {
                self.state.optimistic_prompts.remove(session_id);
            }
        }
        let adjustment = self.widgets.transcript_scroll.vadjustment();
        let old_upper = adjustment.upper();
        let old_value = adjustment.value();
        if transcript_session_changed(self.rendered_session.as_deref(), active.as_deref()) {
            self.pin_transcript_to_bottom();
        }
        let follow_bottom = self.transcript_at_bottom;
        self.sync_transcript_data(active.as_deref(), rows);
        let loading = active
            .as_ref()
            .is_some_and(|active| self.state.loading_messages.contains(active));
        let replacing = active
            .as_ref()
            .is_some_and(|active| self.replacing_messages.contains(active));
        let loaded = active
            .as_ref()
            .and_then(|active| self.state.conversations.get(active))
            .is_some_and(|conversation| conversation.loaded);
        let retry_message = active
            .as_ref()
            .and_then(|active| self.state.statuses.get(active))
            .and_then(|status| match status {
                RunStatus::Retry { message, .. } => Some(message.clone()),
                _ => None,
            });
        let working = active
            .as_ref()
            .is_some_and(|active| self.state.statuses.get(active).is_some_and(|s| s.is_busy()));
        let load_error = active
            .as_ref()
            .and_then(|active| self.message_load_errors.get(active));
        let has_rows = !self.rendered_rows.is_empty();
        let indicator = if let Some(message) = retry_message {
            TranscriptIndicator::Retry(message)
        } else {
            transcript_indicator(
                active.is_some(),
                loading,
                replacing,
                loaded,
                has_rows,
                working,
                load_error.is_some(),
            )
        };
        self.refresh_transcript_indicator(&indicator, has_rows, load_error.map(String::as_str));
        if update == TranscriptUpdate::Activate {
            self.widgets.load_earlier.set_visible(false);
            self.place_sticky_message(false);
        } else {
            self.refresh_load_earlier_visibility();
        }
        self.queue_transcript_edge_refresh();

        let generation = self.transcript_scroll_generation;
        let weak = self.self_weak.clone();
        if update == TranscriptUpdate::Prepend {
            glib::idle_add_local_once(move || {
                let valid = weak.upgrade().is_some_and(|controller| {
                    let controller = controller.borrow();
                    controller.transcript_scroll_generation == generation
                        && controller.state.active == active
                });
                if valid {
                    let value = old_value + (adjustment.upper() - old_upper);
                    adjustment.set_value(clamp_adjustment(&adjustment, value));
                }
            });
        } else if should_follow_transcript(update, follow_bottom) {
            glib::idle_add_local_once(move || {
                let should_follow = weak.upgrade().is_some_and(|controller| {
                    let controller = controller.borrow();
                    controller.transcript_scroll_generation == generation
                        && controller.state.active == active
                });
                if should_follow {
                    scroll_adjustment_to_bottom(&adjustment);
                    let adjustment = adjustment.clone();
                    let weak = weak.clone();
                    glib::timeout_add_local_once(Duration::from_millis(50), move || {
                        let still_active = weak.upgrade().is_some_and(|controller| {
                            let controller = controller.borrow();
                            controller.transcript_scroll_generation == generation
                                && controller.state.active == active
                        });
                        if still_active {
                            scroll_adjustment_to_bottom(&adjustment);
                        }
                    });
                }
            });
        }
    }

    fn refresh_load_earlier_visibility(&self) {
        let active = self.state.active.as_ref();
        let has_earlier = active
            .and_then(|active| self.state.conversations.get(active))
            .is_some_and(|conversation| conversation.next_cursor.is_some());
        let loading = active.is_some_and(|active| self.state.loading_messages.contains(active));
        let replacing = active.is_some_and(|active| self.replacing_messages.contains(active));
        let at_boundary = adjustment_at_top(&self.widgets.transcript_scroll.vadjustment());
        self.widgets
            .load_earlier
            .set_visible(has_earlier && at_boundary);
        self.widgets.load_earlier.set_sensitive(!loading);
        self.widgets
            .load_earlier
            .set_label(if loading && !replacing {
                "Loading earlier messages..."
            } else {
                "Load earlier messages"
            });
    }

    fn refresh_sticky_message(&self) {
        let loaded = self.state.active.as_ref().is_some_and(|active| {
            self.state
                .conversations
                .get(active)
                .is_some_and(|conversation| conversation.loaded)
        });
        if !loaded || self.rendered_rows.is_empty() {
            self.place_sticky_message(false);
            return;
        }
        let adjustment = self.widgets.transcript_scroll.vadjustment();
        if adjustment_at_top(&adjustment) {
            self.place_sticky_message(false);
            return;
        }
        let user_indices: Vec<usize> = self
            .rendered_rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| transcript_row_is_user(row).then_some(index))
            .collect();
        let viewport_bottom = adjustment.page_size();
        if viewport_bottom <= 0.0 {
            self.place_sticky_message(false);
            return;
        }
        let realized = row_layout_bounds(&self.rendered_rows, &self.transcript_heights);
        let sticky_index = sticky_user_index(
            &user_indices,
            &realized,
            adjustment.value(),
            adjustment.value() + viewport_bottom,
        );
        match sticky_index.and_then(|index| {
            self.rendered_rows
                .get(index)
                .and_then(|row| serde_json::from_str::<TranscriptRow>(row).ok())
        }) {
            Some(row) => {
                let time_text = display_local_timestamp(row.time).unwrap_or_default();
                let body = sticky_message_text(row);
                if self.widgets.sticky_message_body.text() != body {
                    self.widgets
                        .sticky_message_scroll
                        .vadjustment()
                        .set_value(0.0);
                    self.widgets.sticky_message_body.set_label(&body);
                }
                self.widgets.sticky_message_time.set_label(&time_text);
                self.widgets
                    .sticky_message_time
                    .set_visible(!time_text.is_empty());
                self.place_sticky_message(true);
            }
            None => {
                self.widgets.sticky_message_body.set_label("");
                self.widgets.sticky_message_time.set_label("");
                self.place_sticky_message(false);
            }
        }
    }

    fn place_sticky_message(&self, visible: bool) {
        let wide_enough = self.widgets.transcript_scroll.width() > ROW_PADDING_X;
        self.widgets
            .sticky_message
            .set_visible(visible && wide_enough);
    }

    fn queue_transcript_edge_refresh(&mut self) {
        self.queue_transcript_relayout(true);
    }

    fn queue_transcript_relayout(&mut self, remesure: bool) {
        self.transcript_edge_remeasure |= remesure;
        if self.transcript_edge_refresh_scheduled {
            return;
        }
        self.transcript_edge_refresh_scheduled = true;
        let weak = self.self_weak.clone();
        glib::idle_add_local_once(move || {
            let Some(controller) = weak.upgrade() else {
                return;
            };
            let mut controller = controller.borrow_mut();
            controller.transcript_edge_refresh_scheduled = false;
            let remesure = std::mem::take(&mut controller.transcript_edge_remeasure);
            controller.relayout_transcript(remesure);
            controller.refresh_load_earlier_visibility();
            controller.refresh_sticky_message();
        });
    }

    fn relayout_transcript(&mut self, remesure: bool) {
        if !self.widgets.transcript_scroll.is_realized() {
            return;
        }
        let width = self.widgets.transcript_scroll.width();
        if width <= ROW_PADDING_X {
            self.recycle_visible_rows();
            return;
        }
        let width_changed = self.transcript_layout_width != width;
        self.transcript_layout_width = width;
        let adjustment = self.widgets.transcript_scroll.vadjustment();
        let page = adjustment.page_size();
        let (start, end) = target_visible_range(
            &self.transcript_heights,
            adjustment.value(),
            page,
            ROW_OVERSCAN,
            self.transcript_at_bottom,
        );
        self.sync_visible_rows(start, end);
        if remesure || width_changed {
            self.refresh_visible_row_heights(width);
        }
        let total = self.transcript_heights.iter().copied().sum::<i32>().max(0);
        let top = row_offset(&self.transcript_heights, start);
        let bottom = (total - row_offset(&self.transcript_heights, end)).max(0);
        let spacer_changed = (self.widgets.transcript_spacer.height_request() - top).abs() >= 2;
        let tail_changed = (self.widgets.transcript_tail.height_request() - bottom).abs() >= 2;
        if spacer_changed {
            self.widgets.transcript_spacer.set_size_request(-1, top);
        }
        if tail_changed {
            self.widgets.transcript_tail.set_size_request(-1, bottom);
        }
        if debug_logging_enabled() && (remesure || width_changed || spacer_changed || tail_changed)
        {
            let adjustment = self.widgets.transcript_scroll.vadjustment();
            debug_log(format!(
                "relayout remesure={remesure} width={width} page={:.1} value={:.1} upper={:.1} rows={start}..{end}/{} spacer={top} tail={bottom} total={total} pinned={}",
                adjustment.page_size(),
                adjustment.value(),
                adjustment.upper(),
                self.transcript_heights.len(),
                self.transcript_at_bottom
            ));
        }
    }

    fn refresh_visible_row_heights(&mut self, width: i32) -> bool {
        let mut changed = false;
        for slot in &self.transcript_visible {
            if slot.row.width_request() != -1 {
                slot.row.set_size_request(-1, -1);
            }
            let measure_width = match slot.row.width() {
                w if w > ROW_PADDING_X => w,
                _ => width,
            };
            let (_, natural, _, _) = slot.row.measure(gtk::Orientation::Vertical, measure_width);
            let height = natural.max(1);
            if let Some(stored) = self.transcript_heights.get_mut(slot.index) {
                if (*stored - height).abs() >= 2 {
                    *stored = height;
                    changed = true;
                }
            }
        }
        changed
    }

    fn sync_transcript_data(&mut self, active_session: Option<&str>, new_rows: Vec<String>) {
        if transcript_session_changed(self.rendered_session.as_deref(), active_session) {
            self.recycle_visible_rows();
            self.transcript_heights.clear();
            self.rendered_rows.clear();
            self.rendered_session = active_session.map(str::to_owned);
        }
        self.transcript_heights =
            reuse_row_heights(&self.rendered_rows, &self.transcript_heights, &new_rows);
        self.rendered_rows = new_rows;
    }

    fn recycle_visible_rows(&mut self) {
        for slot in self.transcript_visible.drain(..) {
            prepare_transcript_row_for_recycle(&self.widgets.window, &slot.row);
            self.widgets.transcript.remove(&slot.row);
            self.transcript_pool.push(slot);
        }
    }

    fn sync_visible_rows(&mut self, start: usize, end: usize) {
        let needed: HashSet<usize> = (start..end).collect();
        let mut keep = Vec::new();
        for slot in self.transcript_visible.drain(..) {
            if needed.contains(&slot.index) {
                keep.push(slot);
            } else {
                prepare_transcript_row_for_recycle(&self.widgets.window, &slot.row);
                self.widgets.transcript.remove(&slot.row);
                self.transcript_pool.push(slot);
            }
        }
        let have: HashSet<usize> = keep.iter().map(|slot| slot.index).collect();
        for index in start..end {
            if have.contains(&index) {
                continue;
            }
            let mut slot = self
                .transcript_pool
                .pop()
                .unwrap_or_else(new_transcript_slot);
            slot.index = index;
            slot.bound.clear();
            slot.row.set_halign(gtk::Align::Fill);
            slot.row.set_hexpand(true);
            slot.row.set_valign(gtk::Align::Start);
            self.widgets.transcript.append(&slot.row);
            keep.push(slot);
        }
        keep.sort_by_key(|slot| slot.index);
        let mut previous: Option<gtk::Widget> = None;
        for slot in &keep {
            self.widgets
                .transcript
                .reorder_child_after(&slot.row, previous.as_ref());
            previous = Some(slot.row.clone().upcast());
        }
        for slot in &mut keep {
            let Some(value) = self.rendered_rows.get(slot.index) else {
                continue;
            };
            if slot.bound != *value {
                bind_transcript_row(&slot.row, value, slot.index as u32);
                slot.bound = value.clone();
            }
        }
        self.transcript_visible = keep;
    }

    fn refresh_transcript_indicator(
        &self,
        indicator: &TranscriptIndicator,
        has_rows: bool,
        error: Option<&str>,
    ) {
        let (label, spinning, is_retry) = match indicator {
            TranscriptIndicator::Hidden => ("", false, false),
            TranscriptIndicator::NoSession => ("Open a session to begin", false, false),
            TranscriptIndicator::Loading => ("Loading conversation", true, false),
            TranscriptIndicator::Refreshing => ("Refreshing conversation", true, false),
            TranscriptIndicator::Working => ("OpenCode is working", true, false),
            TranscriptIndicator::Retry(message) => (message.as_str(), true, true),
            TranscriptIndicator::Error => (
                if has_rows {
                    "Could not refresh conversation"
                } else {
                    "Could not load conversation"
                },
                false,
                false,
            ),
            TranscriptIndicator::Empty => ("No messages yet", false, false),
        };
        let visible = *indicator != TranscriptIndicator::Hidden;
        let compact = visible && has_rows;
        self.widgets.transcript_scroll.set_visible(true);
        self.widgets.transcript_scroll.set_vexpand(true);
        self.widgets.transcript_status.set_visible(visible);
        self.widgets.transcript_status.set_vexpand(false);
        if compact {
            self.widgets
                .transcript_status
                .add_css_class("transcript-status-compact");
        } else {
            self.widgets
                .transcript_status
                .remove_css_class("transcript-status-compact");
        }
        if is_retry {
            self.widgets
                .transcript_status
                .add_css_class("transcript-status-retry");
        } else {
            self.widgets
                .transcript_status
                .remove_css_class("transcript-status-retry");
        }
        self.widgets.transcript_status_label.set_label(label);
        self.widgets.transcript_status.set_tooltip_text(error);
        self.widgets.transcript_spinner.set_visible(spinning);
        if spinning {
            self.widgets.transcript_spinner.start();
        } else {
            self.widgets.transcript_spinner.stop();
        }
    }

    fn update_session_status(&mut self, session_id: &str, status: RunStatus) -> bool {
        let previous = self
            .state
            .statuses
            .insert(session_id.to_owned(), status.clone());
        previous != Some(status)
    }

    fn notify_session_idle(&self, session_id: &str) {
        if self
            .session(session_id)
            .is_some_and(|session| session.parent_id.is_some())
        {
            return;
        }
        let Some(application) = self.widgets.window.application() else {
            return;
        };
        let title = self
            .session(session_id)
            .map(|session| session.title.clone())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "OpenCode".to_owned());
        let notification = gio::Notification::new(&title);
        notification.set_body(Some("opencode session"));
        notification.set_default_action_and_target_value(
            "app.activate-session",
            Some(&session_id.to_variant()),
        );
        application.send_notification(
            Some(&session_notification_id(&self.server_key, session_id)),
            &notification,
        );
        play_session_ready_sound();
    }

    fn clear_session_unread(&mut self, session_id: &str) -> bool {
        let changed = self.state.unread.remove(session_id);
        if changed {
            if let Some(application) = self.widgets.window.application() {
                application
                    .withdraw_notification(&session_notification_id(&self.server_key, session_id));
            }
            self.persist_state();
        }
        changed
    }

    fn acknowledge_active_unread(controller: &Rc<RefCell<Self>>) {
        let mut this = controller.borrow_mut();
        let Some(active) = this.state.active.clone() else {
            return;
        };
        if this.clear_session_unread(&active) {
            let weak = this.self_weak.clone();
            this.refresh_tabs(&weak);
        }
    }

    fn send_if_idle(controller: &Rc<RefCell<Self>>) {
        let running = {
            let this = controller.borrow();
            this.state
                .active
                .as_ref()
                .and_then(|active| this.state.statuses.get(active))
                .is_some_and(|status| status.is_busy())
        };
        if !running {
            Self::send_or_abort(controller);
        }
    }

    fn send_or_abort(controller: &Rc<RefCell<Self>>) {
        let command = {
            let mut this = controller.borrow_mut();
            let Some(active) = this.state.active.clone() else {
                return;
            };
            let Some(session) = this.session(&active).cloned() else {
                return;
            };
            if this
                .state
                .statuses
                .get(&active)
                .is_some_and(|status| status.is_busy())
            {
                if this.state.pending_prompts.contains_key(&active)
                    && !this.state.server_busy.contains(&active)
                {
                    this.state.abort_requested.insert(active);
                    None
                } else {
                    this.state.abort_requested.remove(&active);
                    Some(Command::Abort {
                        session_id: active,
                        directory: session.directory,
                    })
                }
            } else {
                let selection = this.state.selections.get(&active).cloned();
                let supports_attachments = this.selected_model_supports_attachments();
                let draft = this.state.drafts.entry(active.clone()).or_default();
                if draft.text.trim() == "/debug error" {
                    draft.text.clear();
                    this.widgets.composer.buffer().set_text("");
                    let conversation = this.state.conversations.entry(active.clone()).or_default();
                    conversation.apply_event(&serde_json::json!({
                        "type": "session.error",
                        "properties": {
                            "sessionID": active,
                            "error": {
                                "name": "APIError",
                                "data": {
                                    "message": "AI_APICallError: Not Found (404)",
                                    "statusCode": 404
                                }
                            }
                        }
                    }));
                    this.refresh_transcript(TranscriptUpdate::Content);
                    return;
                }
                if draft.text.trim() == "/debug retry" {
                    draft.text.clear();
                    this.widgets.composer.buffer().set_text("");
                    this.update_session_status(
                        &active,
                        RunStatus::Retry {
                            message: "Rate limited upstream. Retrying in 4s (attempt 1/3)..."
                                .into(),
                            attempt: 1,
                        },
                    );
                    this.refresh_transcript(TranscriptUpdate::Content);
                    return;
                }
                if draft.text.trim() == "/debug clear" || draft.text.trim() == "/debug idle" {
                    draft.text.clear();
                    this.widgets.composer.buffer().set_text("");
                    this.update_session_status(&active, RunStatus::Idle);
                    this.refresh_transcript(TranscriptUpdate::Content);
                    return;
                }
                if draft.text.trim().is_empty() && draft.attachments.is_empty() {
                    return;
                }
                if !draft.attachments.is_empty() && !supports_attachments {
                    this.show_error("The selected model does not accept attachments");
                    return;
                }
                let pending = std::mem::take(draft);
                this.next_prompt_request_id += 1;
                let request_id = this.next_prompt_request_id;
                let command = Command::SendPrompt {
                    request_id,
                    session_id: active.clone(),
                    directory: session.directory,
                    text: pending.text.clone(),
                    selection,
                    agent: session.agent,
                    attachments: pending.attachments.clone(),
                };
                let baseline_you = this
                    .state
                    .conversations
                    .get(&active)
                    .map(Conversation::transcript_rows)
                    .unwrap_or_default()
                    .iter()
                    .filter(|row| transcript_row_is_user(row))
                    .count();
                this.state.optimistic_prompts.insert(
                    active.clone(),
                    OptimisticPrompt {
                        row: optimistic_transcript_row(&pending, unix_millis()),
                        baseline_you,
                    },
                );
                this.state.pending_prompts.insert(
                    active.clone(),
                    PendingPrompt {
                        request_id,
                        draft: pending,
                    },
                );
                let unread_changed = this.clear_session_unread(&active);
                let status_changed = this.update_session_status(&active, RunStatus::Busy);
                if status_changed || unread_changed {
                    let weak = this.self_weak.clone();
                    this.refresh_tabs(&weak);
                }
                this.refresh_composer();
                this.pin_transcript_to_bottom();
                this.refresh_transcript(TranscriptUpdate::Content);
                Some(command)
            }
        };
        if let Some(command) = command {
            controller.borrow().api.send(command);
        }
    }

    fn load_earlier(controller: &Rc<RefCell<Self>>) {
        let command = {
            let mut this = controller.borrow_mut();
            let Some(active) = this.state.active.clone() else {
                return;
            };
            let Some(session) = this.session(&active).cloned() else {
                return;
            };
            let Some(before) = this
                .state
                .conversations
                .get(&active)
                .and_then(|conversation| conversation.next_cursor.clone())
            else {
                return;
            };
            if !this.state.loading_messages.insert(active.clone()) {
                return;
            }
            this.refresh_transcript(TranscriptUpdate::Content);
            Command::LoadMessages {
                session_id: active,
                directory: session.directory,
                before: Some(before),
            }
        };
        controller.borrow().api.send(command);
    }

    fn pick_attachments(controller: &Rc<RefCell<Self>>) {
        let (session_id, window) = {
            let this = controller.borrow();
            if !this.selected_model_supports_attachments() {
                return;
            }
            let Some(session_id) = this.state.active.clone() else {
                return;
            };
            (session_id, this.widgets.window.clone())
        };
        let chooser = gtk::FileChooserNative::new(
            Some("Attach files"),
            Some(&window),
            gtk::FileChooserAction::Open,
            Some("Attach"),
            Some("Cancel"),
        );
        chooser.set_select_multiple(true);
        let weak = Rc::downgrade(controller);
        chooser.connect_response(move |chooser, response| {
            if response == gtk::ResponseType::Accept {
                let files = chooser.files();
                let paths: Vec<PathBuf> = (0..files.n_items())
                    .filter_map(|index| files.item(index))
                    .filter_map(|item| item.downcast::<gio::File>().ok())
                    .filter_map(|file| file.path())
                    .collect();
                if let Some(controller) = weak.upgrade() {
                    let mut this = controller.borrow_mut();
                    if this.state.tabs.contains(&session_id) {
                        let draft = this.state.drafts.entry(session_id.clone()).or_default();
                        for path in paths {
                            if !draft.attachments.contains(&path) {
                                draft.attachments.push(path);
                            }
                        }
                        if this.state.active.as_deref() == Some(session_id.as_str()) {
                            this.refresh_attachments();
                            this.refresh_send_button();
                        }
                    }
                }
            }
            chooser.hide();
        });
        chooser.show();
    }

    fn paste_clipboard_image(controller: &Rc<RefCell<Self>>) {
        let (session_id, clipboard) = {
            let mut this = controller.borrow_mut();
            if !this.selected_model_supports_attachments() {
                this.show_error("The selected model does not accept images");
                return;
            }
            let Some(session_id) = this.state.active.clone() else {
                return;
            };
            let Some(display) = gdk::Display::default() else {
                return;
            };
            (session_id, display.clipboard())
        };
        let weak = Rc::downgrade(controller);
        glib::spawn_future_local(async move {
            let result = clipboard
                .read_texture_future()
                .await
                .map_err(|error| error.to_string())
                .and_then(|texture| texture.ok_or_else(|| "Clipboard has no image".to_owned()))
                .and_then(|texture| save_clipboard_texture(&texture));
            let Some(controller) = weak.upgrade() else {
                return;
            };
            let mut this = controller.borrow_mut();
            match result {
                Ok(path) if this.state.tabs.contains(&session_id) => {
                    let draft = this.state.drafts.entry(session_id.clone()).or_default();
                    draft.attachments.push(path);
                    if this.state.active.as_deref() == Some(session_id.as_str()) {
                        this.refresh_attachments();
                        this.refresh_send_button();
                    }
                }
                Ok(path) => remove_clipboard_attachment(&path),
                Err(error) => this.show_error(&format!("Could not paste image: {error}")),
            }
        });
    }

    fn show_model_picker(controller: &Rc<RefCell<Self>>) {
        let (popover, search) = {
            let this = controller.borrow();
            if !this.widgets.model_button.is_sensitive() || this.current_models.is_empty() {
                return;
            }
            (
                this.widgets.model_popover.clone(),
                this.widgets.model_search.clone(),
            )
        };
        search.set_text("");
        Self::filter_model_list(controller, "");
        popover.popup();
        search.grab_focus();
    }

    fn filter_model_list(controller: &Rc<RefCell<Self>>, query: &str) {
        let (current_models, active_selection, list, filtered_indices) = {
            let this = controller.borrow();
            let selection = this.current_model_selection_for_active();
            (
                this.current_models.clone(),
                selection,
                this.widgets.model_list.clone(),
                this.widgets.model_filtered_indices.clone(),
            )
        };

        while let Some(child) = list.first_child() {
            list.remove(&child);
        }

        let query = query.trim();
        let mut scored: Vec<(i64, usize, ModelOption)> = current_models
            .into_iter()
            .enumerate()
            .filter_map(|(idx, model)| {
                if query.is_empty() {
                    Some((0, idx, model))
                } else {
                    let label_score = fuzzy_score(query, &model.label);
                    let full_id = format!("{} / {}", model.provider_id, model.model_id);
                    let id_score = fuzzy_score(query, &full_id);
                    let score = match (label_score, id_score) {
                        (Some(s1), Some(s2)) => Some(s1.max(s2)),
                        (s1, s2) => s1.or(s2),
                    };
                    score.map(|s| (s, idx, model))
                }
            })
            .collect();

        if !query.is_empty() {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.label.cmp(&b.2.label)));
        }

        let mut indices = Vec::with_capacity(scored.len());

        if scored.is_empty() {
            *filtered_indices.borrow_mut() = Vec::new();
            let empty = gtk::Label::new(Some("No matching models"));
            empty.add_css_class("model-picker-empty");
            empty.set_margin_top(16);
            empty.set_margin_bottom(16);
            list.append(&empty);
            return;
        }

        let mut row_to_select = None;

        for (rank, (_, idx, model)) in scored.iter().enumerate() {
            indices.push(*idx);

            let is_selected = active_selection.as_ref().is_some_and(|sel| {
                sel.provider_id == model.provider_id && sel.model_id == model.model_id
            });

            let row = gtk::ListBoxRow::new();
            row.add_css_class("model-picker-row");
            if is_selected {
                row.add_css_class("selected");
            }

            let box_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            box_row.set_margin_start(10);
            box_row.set_margin_end(10);
            box_row.set_margin_top(6);
            box_row.set_margin_bottom(6);

            let labels_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
            labels_box.set_hexpand(true);

            let label = gtk::Label::new(Some(&model.label));
            label.set_xalign(0.0);
            label.add_css_class("model-picker-item-title");

            let subtext =
                gtk::Label::new(Some(&format!("{}/{}", model.provider_id, model.model_id)));
            subtext.set_xalign(0.0);
            subtext.add_css_class("model-picker-item-subtext");

            labels_box.append(&label);
            labels_box.append(&subtext);
            box_row.append(&labels_box);

            if is_selected {
                let check = gtk::Label::new(Some("✓"));
                check.add_css_class("model-picker-check");
                box_row.append(&check);
            }

            row.set_child(Some(&box_row));
            list.append(&row);

            if is_selected || (rank == 0 && row_to_select.is_none()) {
                row_to_select = Some(row.clone());
            }
        }

        if let Some(row) = row_to_select {
            list.select_row(Some(&row));
        }

        *filtered_indices.borrow_mut() = indices;
    }

    fn activate_current_model_selection(controller: &Rc<RefCell<Self>>) {
        let (list, filtered_indices) = {
            let this = controller.borrow();
            (
                this.widgets.model_list.clone(),
                this.widgets.model_filtered_indices.clone(),
            )
        };
        let row_idx = list
            .selected_row()
            .map(|row| row.index() as usize)
            .or_else(|| {
                if list.row_at_index(0).is_some() {
                    Some(0)
                } else {
                    None
                }
            });
        let model_idx = row_idx
            .and_then(|idx| filtered_indices.borrow().get(idx).copied())
            .or_else(|| filtered_indices.borrow().first().copied());
        if let Some(model_idx) = model_idx {
            Self::select_model(controller, model_idx);
        }
    }

    fn select_model(controller: &Rc<RefCell<Self>>, index: usize) {
        let _active = {
            let mut this = controller.borrow_mut();
            let Some(active) = this.state.active.clone() else {
                return;
            };
            let Some(model) = this.current_models.get(index).cloned() else {
                return;
            };
            this.state.selections.insert(
                active.clone(),
                ModelSelection {
                    provider_id: model.provider_id,
                    model_id: model.model_id,
                    variant: None,
                },
            );
            active
        };
        let mut this = controller.borrow_mut();
        this.refresh_model_control();
        this.refresh_variant_control();
        this.refresh_attachment_control();
        this.refresh_context_usage();
        this.persist_state();
        this.refresh_send_button();
        this.widgets.model_popover.popdown();
        this.widgets.composer.grab_focus();
    }

    fn show_variant_picker(controller: &Rc<RefCell<Self>>) {
        let (popover, search) = {
            let this = controller.borrow();
            if !this.widgets.variant_button.is_sensitive() || this.current_variants.len() <= 1 {
                return;
            }
            (
                this.widgets.variant_popover.clone(),
                this.widgets.variant_search.clone(),
            )
        };
        search.set_text("");
        Self::filter_variant_list(controller, "");
        popover.popup();
        search.grab_focus();
    }

    fn filter_variant_list(controller: &Rc<RefCell<Self>>, query: &str) {
        let (current_variants, active_selection, list, filtered_indices) = {
            let this = controller.borrow();
            let selection = this.current_model_selection_for_active();
            (
                this.current_variants.clone(),
                selection,
                this.widgets.variant_list.clone(),
                this.widgets.variant_filtered_indices.clone(),
            )
        };

        while let Some(child) = list.first_child() {
            list.remove(&child);
        }

        let query = query.trim();
        let mut scored: Vec<(i64, usize, Option<String>)> = current_variants
            .into_iter()
            .enumerate()
            .filter_map(|(idx, variant)| {
                let label = variant.as_deref().unwrap_or("Default");
                if query.is_empty() {
                    Some((0, idx, variant))
                } else {
                    let score = fuzzy_score(query, label);
                    score.map(|s| (s, idx, variant))
                }
            })
            .collect();

        if !query.is_empty() {
            scored.sort_by(|a, b| {
                b.0.cmp(&a.0).then_with(|| {
                    let la = a.2.as_deref().unwrap_or("Default");
                    let lb = b.2.as_deref().unwrap_or("Default");
                    la.cmp(lb)
                })
            });
        }

        let mut indices = Vec::with_capacity(scored.len());

        if scored.is_empty() {
            *filtered_indices.borrow_mut() = Vec::new();
            let empty = gtk::Label::new(Some("No matching levels"));
            empty.add_css_class("model-picker-empty");
            empty.set_margin_top(14);
            empty.set_margin_bottom(14);
            list.append(&empty);
            return;
        }

        let mut row_to_select = None;

        for (rank, (_, idx, variant)) in scored.iter().enumerate() {
            indices.push(*idx);

            let is_selected = match (&active_selection, &variant) {
                (Some(sel), Some(v)) => sel.variant.as_deref() == Some(v.as_str()),
                (Some(sel), None) => sel.variant.is_none(),
                _ => false,
            };

            let row = gtk::ListBoxRow::new();
            row.add_css_class("model-picker-row");
            if is_selected {
                row.add_css_class("selected");
            }

            let box_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            box_row.set_margin_start(10);
            box_row.set_margin_end(10);
            box_row.set_margin_top(6);
            box_row.set_margin_bottom(6);

            let label = gtk::Label::new(Some(variant.as_deref().unwrap_or("Default")));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.add_css_class("model-picker-item-title");

            box_row.append(&label);

            if is_selected {
                let check = gtk::Label::new(Some("✓"));
                check.add_css_class("model-picker-check");
                box_row.append(&check);
            }

            row.set_child(Some(&box_row));
            list.append(&row);

            if is_selected || (rank == 0 && row_to_select.is_none()) {
                row_to_select = Some(row.clone());
            }
        }

        if let Some(row) = row_to_select {
            list.select_row(Some(&row));
        }

        *filtered_indices.borrow_mut() = indices;
    }

    fn activate_current_variant_selection(controller: &Rc<RefCell<Self>>) {
        let (list, filtered_indices) = {
            let this = controller.borrow();
            (
                this.widgets.variant_list.clone(),
                this.widgets.variant_filtered_indices.clone(),
            )
        };
        let row_idx = list
            .selected_row()
            .map(|row| row.index() as usize)
            .or_else(|| {
                if list.row_at_index(0).is_some() {
                    Some(0)
                } else {
                    None
                }
            });
        let variant_idx = row_idx
            .and_then(|idx| filtered_indices.borrow().get(idx).copied())
            .or_else(|| filtered_indices.borrow().first().copied());
        if let Some(variant_idx) = variant_idx {
            Self::select_variant(controller, variant_idx);
        }
    }

    fn select_variant(controller: &Rc<RefCell<Self>>, index: usize) {
        let _active = {
            let mut this = controller.borrow_mut();
            let Some(active) = this.state.active.clone() else {
                return;
            };
            let variant = this.current_variants.get(index).cloned().flatten();
            if let Some(selection) = this.state.selections.get_mut(&active) {
                selection.variant = variant;
            } else if let Some(mut selection) = this.current_model_selection_for_active() {
                selection.variant = variant;
                this.state.selections.insert(active.clone(), selection);
            }
            active
        };
        let mut this = controller.borrow_mut();
        this.refresh_variant_control();
        this.persist_state();
        this.widgets.variant_popover.popdown();
        this.widgets.composer.grab_focus();
    }

    fn show_session_picker(controller: &Rc<RefCell<Self>>) {
        {
            let this = controller.borrow();
            if this.app_modal == Some(AppModalKind::Sessions) {
                if let Some((list, search)) = &this.session_picker {
                    let sessions = this.tab_sessions();
                    let active = this.state.active.clone();
                    populate_session_list(
                        list,
                        &sessions,
                        active.as_deref(),
                        search.text().as_str(),
                        Rc::downgrade(controller),
                    );
                    this.widgets.app_modal_overlay.set_visible(true);
                    search.grab_focus();
                    return;
                }
            }
        }
        let (root, list, search) = sessions_picker_body();

        search.connect_changed({
            let list = list.clone();
            let weak = Rc::downgrade(controller);
            move |search| {
                if let Some(controller) = weak.upgrade() {
                    let this = controller.borrow();
                    let sessions = this.tab_sessions();
                    let active = this.state.active.clone();
                    populate_session_list(
                        &list,
                        &sessions,
                        active.as_deref(),
                        search.text().as_str(),
                        Rc::downgrade(&controller),
                    );
                }
            }
        });
        search.connect_activate({
            let list = list.clone();
            move |_| {
                if let Some(first_child) = list.first_child() {
                    if let Ok(button) = first_child.downcast::<gtk::Button>() {
                        button.emit_clicked();
                    }
                }
            }
        });
        let search_keys = gtk::EventControllerKey::new();
        search_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        search_keys.connect_key_pressed({
            let list = list.clone();
            move |_, key, _, _| match key {
                gdk::Key::Down => {
                    if let Some(first_child) = list.first_child() {
                        first_child.grab_focus();
                        glib::Propagation::Stop
                    } else {
                        glib::Propagation::Proceed
                    }
                }
                _ => glib::Propagation::Proceed,
            }
        });
        search.add_controller(search_keys);

        let list_keys = gtk::EventControllerKey::new();
        list_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        list_keys.connect_key_pressed({
            let list = list.clone();
            let search = search.clone();
            move |_, key, _, _| match key {
                gdk::Key::Up => {
                    let mut prev: Option<gtk::Widget> = None;
                    let mut child = list.first_child();
                    let mut focused_is_first = false;
                    while let Some(c) = child {
                        if c.has_focus() || c.is_focus() {
                            if prev.is_none() {
                                focused_is_first = true;
                            }
                            break;
                        }
                        prev = Some(c.clone());
                        child = c.next_sibling();
                    }
                    if focused_is_first {
                        search.grab_focus();
                        glib::Propagation::Stop
                    } else if let Some(p) = prev {
                        p.grab_focus();
                        glib::Propagation::Stop
                    } else {
                        glib::Propagation::Proceed
                    }
                }
                gdk::Key::Down => {
                    let mut child = list.first_child();
                    while let Some(c) = child {
                        if c.has_focus() || c.is_focus() {
                            if let Some(next) = c.next_sibling() {
                                next.grab_focus();
                                return glib::Propagation::Stop;
                            }
                            break;
                        }
                        child = c.next_sibling();
                    }
                    glib::Propagation::Proceed
                }
                _ => glib::Propagation::Proceed,
            }
        });
        list.add_controller(list_keys);

        {
            let mut this = controller.borrow_mut();
            this.open_app_modal(AppModalKind::Sessions, 420, 310);
            let sessions = this.tab_sessions();
            let active = this.state.active.clone();
            populate_session_list(
                &list,
                &sessions,
                active.as_deref(),
                "",
                Rc::downgrade(controller),
            );
            this.widgets.app_modal_card.append(&root);
            this.app_modal_focus = Some(search.clone().upcast());
            this.session_picker = Some((list.clone(), search.clone()));
        }
        search.grab_focus();
    }

    fn show_settings(controller: &Rc<RefCell<Self>>, initial_tab: Option<&str>) {
        {
            let this = controller.borrow();
            if this.app_modal == Some(AppModalKind::Settings) {
                this.widgets.app_modal_overlay.set_visible(true);
                if let Some(focus) = &this.app_modal_focus {
                    focus.grab_focus();
                }
                return;
            }
        }
        let config = controller.borrow().connection_config.clone();

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.set_hexpand(true);
        root.set_vexpand(true);

        // Left rail (Variant 3: Minimalist Tabular Row, 170px width)
        let rail = gtk::Box::new(gtk::Orientation::Vertical, 2);
        rail.add_css_class("settings-rail");
        rail.set_size_request(170, -1);
        rail.set_vexpand(true);

        let rail_heading = gtk::Label::new(Some("Settings"));
        rail_heading.set_xalign(0.0);
        rail_heading.set_margin_start(8);
        rail_heading.set_margin_top(4);
        rail_heading.set_margin_bottom(12);
        rail_heading.add_css_class("modal-heading");
        rail.append(&rail_heading);

        let btn_connection = gtk::Button::new();
        btn_connection.add_css_class("flat");
        btn_connection.add_css_class("settings-rail-item");
        let conn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let conn_icon = icon_image(ICON_CONNECTION, 14);
        let conn_text = gtk::Label::new(Some("Connection"));
        conn_text.set_hexpand(true);
        conn_text.set_xalign(0.0);
        conn_row.append(&conn_icon);
        conn_row.append(&conn_text);
        btn_connection.set_child(Some(&conn_row));

        let session_count = controller.borrow().state.sessions.len();
        let btn_sessions = gtk::Button::new();
        btn_sessions.add_css_class("flat");
        btn_sessions.add_css_class("settings-rail-item");
        let sess_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let sess_icon = icon_image(ICON_SESSIONS, 14);
        let sess_text = gtk::Label::new(Some("Sessions"));
        sess_text.set_hexpand(true);
        sess_text.set_xalign(0.0);
        sess_row.append(&sess_icon);
        sess_row.append(&sess_text);
        if session_count > 0 {
            let count_badge = gtk::Label::new(Some(&session_count.to_string()));
            count_badge.add_css_class("rail-badge");
            count_badge.set_xalign(1.0);
            sess_row.append(&count_badge);
        }
        btn_sessions.set_child(Some(&sess_row));

        rail.append(&btn_connection);
        rail.append(&btn_sessions);

        let rail_footer = gtk::Box::new(gtk::Orientation::Vertical, 2);
        rail_footer.add_css_class("settings-rail-footer");
        rail_footer.set_vexpand(true);
        rail_footer.set_valign(gtk::Align::End);
        let host_name = url::Url::parse(&config.base_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_else(|| "remote".to_owned());
        let host_label = gtk::Label::new(Some(&host_name));
        host_label.set_xalign(0.0);
        host_label.set_ellipsize(pango::EllipsizeMode::Middle);
        let version_label = gtk::Label::new(Some("opencode-gtk v0.1.0"));
        version_label.set_xalign(0.0);
        rail_footer.append(&host_label);
        rail_footer.append(&version_label);
        rail.append(&rail_footer);

        // Stack for pages
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(150);

        // --- Page 1: Connection ---
        let connection_page = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let conn_topbar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        conn_topbar.add_css_class("settings-topbar");
        let conn_titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let conn_title = gtk::Label::new(Some("Server Connection"));
        conn_title.set_xalign(0.0);
        conn_title.add_css_class("modal-heading");
        let conn_sub = gtk::Label::new(Some(
            "Configure server endpoint, credentials, and Cloudflare tokens",
        ));
        conn_sub.set_xalign(0.0);
        conn_sub.add_css_class("session-picker-path");
        conn_titles.append(&conn_title);
        conn_titles.append(&conn_sub);
        conn_titles.set_hexpand(true);
        conn_topbar.append(&conn_titles);
        let conn_esc = gtk::Label::new(Some("Esc to cancel"));
        conn_esc.add_css_class("session-picker-time");
        conn_topbar.append(&conn_esc);
        connection_page.append(&conn_topbar);

        let connection_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .vexpand(true)
            .hexpand(true)
            .build();
        let connection_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
        connection_box.set_margin_top(16);
        connection_box.set_margin_bottom(16);
        connection_box.set_margin_start(24);
        connection_box.set_margin_end(24);
        connection_scroll.set_child(Some(&connection_box));

        let server_label = gtk::Label::new(Some("OpenCode server URL"));
        server_label.set_xalign(0.0);
        let server = gtk::Entry::new();
        server.set_text(&config.base_url);
        server.set_placeholder_text(Some("https://opencode.example.com"));
        let username_label = gtk::Label::new(Some("Username"));
        username_label.set_xalign(0.0);
        let username = gtk::Entry::new();
        username.set_text(&config.username);
        let password_label = gtk::Label::new(Some("Password"));
        password_label.set_xalign(0.0);
        let password = gtk::Entry::new();
        password.set_visibility(false);
        password.set_placeholder_text(Some(if config.password.is_some() {
            "Leave blank to keep the current password"
        } else {
            "Not saved to disk"
        }));
        let connection_hint = gtk::Label::new(Some(
            "Remote servers require HTTPS. Loopback HTTP is supported for SSH tunnels. Passwords stay in memory only.",
        ));
        connection_hint.set_xalign(0.0);
        connection_hint.set_wrap(true);
        connection_hint.add_css_class("session-picker-path");

        let cloudflare_label = gtk::Label::new(Some("Cloudflare Access service token"));
        cloudflare_label.set_xalign(0.0);
        let cloudflare_client_id_label = gtk::Label::new(Some("Client ID"));
        cloudflare_client_id_label.set_xalign(0.0);
        let cloudflare_client_id = gtk::Entry::new();
        if let Some(credentials) = &config.cloudflare_access {
            cloudflare_client_id.set_text(&credentials.client_id);
        }
        cloudflare_client_id.set_placeholder_text(Some("Optional"));
        let cloudflare_client_secret_label = gtk::Label::new(Some("Client secret"));
        cloudflare_client_secret_label.set_xalign(0.0);
        let cloudflare_client_secret = gtk::Entry::new();
        cloudflare_client_secret.set_visibility(false);
        cloudflare_client_secret.set_placeholder_text(Some(
            if config.cloudflare_access.is_some() {
                "Stored in the system keyring"
            } else {
                "Optional"
            },
        ));
        let cloudflare_hint = gtk::Label::new(Some(
            "The token is sent only to HTTPS servers and stored in the Linux system keyring. Clear the client ID to remove it.",
        ));
        cloudflare_hint.set_xalign(0.0);
        cloudflare_hint.set_wrap(true);
        cloudflare_hint.add_css_class("session-picker-path");

        let validation = gtk::Label::new(None);
        validation.set_xalign(0.0);
        validation.set_wrap(true);
        validation.add_css_class("error");
        let cancel = gtk::Button::with_mnemonic("_Cancel");
        let apply = gtk::Button::with_mnemonic("_Apply");
        apply.add_css_class("suggested-action");

        connection_box.append(&server_label);
        connection_box.append(&server);
        connection_box.append(&username_label);
        connection_box.append(&username);
        connection_box.append(&password_label);
        connection_box.append(&password);
        connection_box.append(&connection_hint);
        connection_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        connection_box.append(&cloudflare_label);
        connection_box.append(&cloudflare_client_id_label);
        connection_box.append(&cloudflare_client_id);
        connection_box.append(&cloudflare_client_secret_label);
        connection_box.append(&cloudflare_client_secret);
        connection_box.append(&cloudflare_hint);
        connection_box.append(&validation);
        server.set_activates_default(true);
        username.set_activates_default(true);
        password.set_activates_default(true);
        cloudflare_client_id.set_activates_default(true);
        cloudflare_client_secret.set_activates_default(true);
        server_label.set_mnemonic_widget(Some(&server));
        username_label.set_mnemonic_widget(Some(&username));
        password_label.set_mnemonic_widget(Some(&password));
        cloudflare_client_id_label.set_mnemonic_widget(Some(&cloudflare_client_id));
        cloudflare_client_secret_label.set_mnemonic_widget(Some(&cloudflare_client_secret));

        connection_page.append(&connection_scroll);

        let conn_bottombar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        conn_bottombar.add_css_class("settings-bottombar");
        let mem_hint = gtk::Label::new(Some("Passwords stay in memory only"));
        mem_hint.set_xalign(0.0);
        mem_hint.add_css_class("session-picker-path");
        mem_hint.set_hexpand(true);
        conn_bottombar.append(&mem_hint);
        conn_bottombar.append(&cancel);
        conn_bottombar.append(&apply);
        connection_page.append(&conn_bottombar);

        cancel.connect_clicked({
            let weak = Rc::downgrade(controller);
            move |_| {
                if let Some(controller) = weak.upgrade() {
                    controller.borrow_mut().close_app_modal();
                }
            }
        });
        apply.connect_clicked({
            let weak = Rc::downgrade(controller);
            let server = server.clone();
            let username = username.clone();
            let password = password.clone();
            let cloudflare_client_id = cloudflare_client_id.clone();
            let cloudflare_client_secret = cloudflare_client_secret.clone();
            let validation = validation.clone();
            move |_| {
                let Some(controller) = weak.upgrade() else {
                    return;
                };
                let base_url = server.text().trim().to_owned();
                let username_value = username.text().trim().to_owned();
                if base_url.is_empty() || username_value.is_empty() {
                    validation.set_label("Server URL and username are required");
                    return;
                }
                let current = controller.borrow().connection_config.clone();
                let same_server = same_server(&current.base_url, &base_url);
                let same_identity = same_server && current.username == username_value;
                let entered_password = password.text().to_string();
                let cloudflare_access = match configured_cloudflare_credentials(
                    &current,
                    &base_url,
                    cloudflare_client_id.text().as_str(),
                    cloudflare_client_secret.text().as_str(),
                ) {
                    Ok(credentials) => credentials,
                    Err(error) => {
                        validation.set_label(&error.to_string());
                        return;
                    }
                };
                let config = ApiConfig {
                    base_url,
                    username: username_value,
                    password: if entered_password.is_empty() && same_identity {
                        current.password.clone()
                    } else {
                        (!entered_password.is_empty()).then_some(entered_password)
                    },
                    cloudflare_access,
                };

                if config == controller.borrow().connection_config {
                    if let Err(error) = persist_cloudflare_credentials(&current, &config) {
                        validation.set_label(&error.to_string());
                        return;
                    }
                    let mut this = controller.borrow_mut();
                    this.apply_preferences(&config);
                    this.close_app_modal();
                    return;
                }

                match ApiHandle::start(config.clone()) {
                    Ok((api, events, server_key)) => {
                        if let Err(error) = persist_cloudflare_credentials(&current, &config) {
                            events.close();
                            validation.set_label(&error.to_string());
                            return;
                        }
                        Self::switch_connection(&controller, config, api, events, server_key);
                    }
                    Err(error) => validation.set_label(&error.to_string()),
                }
            }
        });
        for entry in [
            server.clone(),
            username.clone(),
            password.clone(),
            cloudflare_client_id.clone(),
            cloudflare_client_secret.clone(),
        ] {
            let apply = apply.clone();
            entry.connect_activate(move |_| apply.emit_clicked());
        }
        let settings_shortcuts = gtk::EventControllerKey::new();
        settings_shortcuts.set_propagation_phase(gtk::PropagationPhase::Capture);
        settings_shortcuts.connect_key_pressed({
            let apply = apply.clone();
            move |_, key, _, modifiers| {
                if key == gdk::Key::Escape {
                    return glib::Propagation::Proceed;
                }
                if modifiers.contains(gdk::ModifierType::CONTROL_MASK)
                    && matches!(key, gdk::Key::Return | gdk::Key::KP_Enter)
                {
                    apply.emit_clicked();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        });
        connection_box.add_controller(settings_shortcuts);

        // --- Page 2: Sessions Search ---
        let sessions_page = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let sess_topbar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        sess_topbar.add_css_class("settings-topbar");
        let sess_titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let sess_title = gtk::Label::new(Some("All Sessions"));
        sess_title.set_xalign(0.0);
        sess_title.add_css_class("modal-heading");
        let sess_sub = gtk::Label::new(Some(&format!(
            "Search all {session_count} workspace sessions on this server"
        )));
        sess_sub.set_xalign(0.0);
        sess_sub.add_css_class("session-picker-path");
        sess_titles.append(&sess_title);
        sess_titles.append(&sess_sub);
        sess_titles.set_hexpand(true);
        sess_topbar.append(&sess_titles);
        let sess_esc = gtk::Label::new(Some("Esc to close"));
        sess_esc.add_css_class("session-picker-time");
        sess_topbar.append(&sess_esc);
        sessions_page.append(&sess_topbar);

        let search_toolbar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        search_toolbar.set_margin_top(14);
        search_toolbar.set_margin_bottom(8);
        search_toolbar.set_margin_start(20);
        search_toolbar.set_margin_end(20);

        let search = gtk::Entry::new();
        search.set_icon_from_icon_name(gtk::EntryIconPosition::Primary, Some(ICON_SEARCH));
        search.set_placeholder_text(Some("Search sessions by title or path..."));
        search.add_css_class("settings-search");
        search_toolbar.append(&search);
        sessions_page.append(&search_toolbar);

        let session_list = gtk::ListBox::new();
        session_list.set_selection_mode(gtk::SelectionMode::None);
        session_list.add_css_class("new-session-list");
        session_list.set_margin_start(14);
        session_list.set_margin_end(14);

        let session_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .child(&session_list)
            .build();
        sessions_page.append(&session_scroll);

        let sess_bottombar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        sess_bottombar.add_css_class("settings-bottombar");
        let nav_hint = gtk::Label::new(Some("↑ ↓ to navigate · Enter to open in tab"));
        nav_hint.set_xalign(0.0);
        nav_hint.add_css_class("session-picker-path");
        nav_hint.set_hexpand(true);
        let close_btn = gtk::Button::with_mnemonic("_Close");
        let weak_close = Rc::downgrade(controller);
        close_btn.connect_clicked(move |_| {
            if let Some(c) = weak_close.upgrade() {
                c.borrow_mut().close_app_modal();
            }
        });
        sess_bottombar.append(&nav_hint);
        sess_bottombar.append(&close_btn);
        sessions_page.append(&sess_bottombar);

        // Wire search & list interactions
        let session_list_ref = session_list.clone();
        let weak_search = Rc::downgrade(controller);
        search.connect_changed(move |search| {
            if let Some(c) = weak_search.upgrade() {
                let this = c.borrow();
                populate_all_sessions_list(
                    &session_list_ref,
                    &this.state.sessions,
                    &this.state.tabs,
                    search.text().as_str(),
                    Rc::downgrade(&c),
                );
            }
        });

        let session_list_activate = session_list.clone();
        search.connect_activate(move |_| {
            if let Some(first_child) = session_list_activate.first_child() {
                if let Ok(button) = first_child.downcast::<gtk::Button>() {
                    button.emit_clicked();
                }
            }
        });

        let search_keys = gtk::EventControllerKey::new();
        search_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let list_down = session_list.clone();
        search_keys.connect_key_pressed(move |_, key, _, _| match key {
            gdk::Key::Down => {
                if let Some(first_child) = list_down.first_child() {
                    first_child.grab_focus();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
            _ => glib::Propagation::Proceed,
        });
        search.add_controller(search_keys);

        let list_keys = gtk::EventControllerKey::new();
        list_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let list_up = session_list.clone();
        let search_up = search.clone();
        list_keys.connect_key_pressed(move |_, key, _, _| match key {
            gdk::Key::Up => {
                let mut prev: Option<gtk::Widget> = None;
                let mut child = list_up.first_child();
                let mut focused_is_first = false;
                while let Some(c) = child {
                    if c.has_focus() || c.is_focus() {
                        if prev.is_none() {
                            focused_is_first = true;
                        }
                        break;
                    }
                    prev = Some(c.clone());
                    child = c.next_sibling();
                }
                if focused_is_first {
                    search_up.grab_focus();
                    glib::Propagation::Stop
                } else if let Some(p) = prev {
                    p.grab_focus();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
            gdk::Key::Down => {
                let mut child = list_up.first_child();
                while let Some(c) = child {
                    if c.has_focus() || c.is_focus() {
                        if let Some(next) = c.next_sibling() {
                            next.grab_focus();
                            return glib::Propagation::Stop;
                        }
                        break;
                    }
                    child = c.next_sibling();
                }
                glib::Propagation::Proceed
            }
            _ => glib::Propagation::Proceed,
        });
        session_list.add_controller(list_keys);

        stack.add_named(&connection_page, Some("connection"));
        stack.add_named(&sessions_page, Some("sessions"));

        root.append(&rail);
        root.append(&stack);

        // Rail button clicks
        let btn_conn_clone = btn_connection.clone();
        let btn_sess_clone = btn_sessions.clone();
        let stack_conn = stack.clone();
        let server_focus = server.clone();
        let apply_clone = apply.clone();
        let weak_conn = Rc::downgrade(controller);
        btn_connection.connect_clicked(move |_| {
            btn_conn_clone.add_css_class("active");
            btn_sess_clone.remove_css_class("active");
            stack_conn.set_visible_child_name("connection");
            server_focus.grab_focus();
            if let Some(c) = weak_conn.upgrade() {
                c.borrow()
                    .widgets
                    .window
                    .set_default_widget(Some(&apply_clone));
            }
        });

        let btn_conn_clone2 = btn_connection.clone();
        let btn_sess_clone2 = btn_sessions.clone();
        let stack_sess = stack.clone();
        let search_focus = search.clone();
        let weak_sess = Rc::downgrade(controller);
        btn_sessions.connect_clicked(move |_| {
            btn_sess_clone2.add_css_class("active");
            btn_conn_clone2.remove_css_class("active");
            stack_sess.set_visible_child_name("sessions");
            search_focus.grab_focus();
            if let Some(c) = weak_sess.upgrade() {
                c.borrow()
                    .widgets
                    .window
                    .set_default_widget(None::<&gtk::Widget>);
            }
        });

        let start_sessions = initial_tab == Some("sessions");
        if start_sessions {
            btn_sessions.add_css_class("active");
            stack.set_visible_child_name("sessions");
        } else {
            btn_connection.add_css_class("active");
            stack.set_visible_child_name("connection");
        }

        {
            let mut this = controller.borrow_mut();
            this.open_app_modal(AppModalKind::Settings, 820, 640);
            populate_all_sessions_list(
                &session_list,
                &this.state.sessions,
                &this.state.tabs,
                "",
                Rc::downgrade(controller),
            );
            if !start_sessions {
                this.widgets.window.set_default_widget(Some(&apply));
                this.app_modal_focus = Some(server.clone().upcast());
            } else {
                this.app_modal_focus = Some(search.clone().upcast());
            }
            this.widgets.app_modal_card.append(&root);
        }
        if start_sessions {
            search.grab_focus();
        } else {
            server.grab_focus();
        }
    }

    fn apply_preferences(&mut self, config: &ApiConfig) {
        self.connection_config = config.clone();
        self.persisted.connection = ConnectionSettings {
            server: config.base_url.clone(),
            username: config.username.clone(),
            cloudflare_access: config.cloudflare_access.is_some(),
        };
        self.credential_warning = None;
        self.persist_state();
    }

    fn switch_connection(
        controller: &Rc<RefCell<Self>>,
        config: ApiConfig,
        api: ApiHandle,
        events: Receiver<UiEvent>,
        server_key: String,
    ) {
        let generation = {
            let mut this = controller.borrow_mut();
            this.persist_state();
            if let Some(application) = this.widgets.window.application() {
                for session_id in &this.state.unread {
                    application.withdraw_notification(&session_notification_id(
                        &this.server_key,
                        session_id,
                    ));
                }
            }
            this.events.close();
            this.connection_generation += 1;
            let generation = this.connection_generation;
            this.dialogs.clear();
            this.composer_prompts.clear();
            this.shown_composer_prompt = None;
            clear_box(&this.widgets.prompt_host);
            this.widgets
                .composer_stack
                .set_visible_child_name("composer");
            this.close_new_session_overlay();
            this.close_app_modal();

            this.api = api;
            this.events = events.clone();
            this.server_key = server_key.clone();
            let server_state = this
                .persisted
                .servers
                .get(&server_key)
                .cloned()
                .unwrap_or_default();
            this.had_server_state = this.persisted.servers.contains_key(&server_key);
            this.offline_busy = server_state.busy.clone();
            this.connection_config = config.clone();
            this.persisted.connection = ConnectionSettings {
                server: config.base_url,
                username: config.username,
                cloudflare_access: config.cloudflare_access.is_some(),
            };
            this.credential_warning = None;
            this.state = restored_state(server_state);
            this.rendered_session = None;
            this.rendered_rows.clear();
            this.pin_transcript_to_bottom();
            this.current_models.clear();
            this.current_variants.clear();
            this.pending_events.clear();
            this.event_flush_scheduled = false;
            this.session_events_during_bootstrap.clear();
            this.session_removals_during_bootstrap.clear();
            this.status_events_during_bootstrap.clear();
            this.resolved_requests_during_bootstrap.clear();
            this.message_events_during_load.clear();
            this.replacing_messages.clear();
            this.message_reload_pending.clear();
            this.message_load_errors.clear();
            this.models_reload_pending.clear();
            this.model_load_errors.clear();
            this.pending_actions.clear();
            this.bootstrap_pending = true;
            this.bootstrap_reload_pending = false;
            this.bootstrap_dialogs_at_start.clear();
            this.bootstrap_retry_token += 1;
            this.bootstrap_retry_delay = BOOTSTRAP_RETRY_MIN;
            this.pending_session_request = None;
            this.pending_rename_request = None;
            this.connected_once = false;
            this.event_connected = false;
            this.widgets.status.remove_css_class("error");
            this.widgets
                .status
                .set_label(&format!("Connecting to {server_key}"));
            this.widgets.status.set_tooltip_text(Some(&server_key));
            this.persist_state();
            generation
        };
        Self::refresh_all(controller);
        start_event_loop(controller, events, generation);
        controller.borrow().api.send(Command::Bootstrap);
    }

    fn show_new_session(controller: &Rc<RefCell<Self>>) {
        let (overlay, search) = {
            let this = controller.borrow();
            (
                this.widgets.new_session_overlay.clone(),
                this.widgets.new_session_search.clone(),
            )
        };
        search.set_text("");
        Self::filter_new_session_projects(controller, "");
        overlay.set_visible(true);
        search.grab_focus();
    }

    fn filter_new_session_projects(controller: &Rc<RefCell<Self>>, query: &str) {
        let (projects, active_dir, list, filtered_paths) = {
            let this = controller.borrow();
            let known = project_paths(&this.state.projects, &this.state.sessions);
            let active = this.active_directory();
            (
                known,
                active,
                this.widgets.new_session_list.clone(),
                this.widgets.new_session_filtered_paths.clone(),
            )
        };

        while let Some(child) = list.first_child() {
            list.remove(&child);
        }

        let query = query.trim();
        let mut scored: Vec<(i64, String, String)> = projects
            .into_iter()
            .filter_map(|path| {
                let name = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&path)
                    .to_owned();
                if query.is_empty() {
                    let priority = if active_dir.as_deref() == Some(&path) {
                        1000
                    } else {
                        0
                    };
                    Some((priority, name, path))
                } else {
                    let name_score = fuzzy_score(query, &name);
                    let path_score = fuzzy_score(query, &path);
                    let score = match (name_score, path_score) {
                        (Some(s1), Some(s2)) => Some(s1.max(s2)),
                        (s1, s2) => s1.or(s2),
                    };
                    score.map(|s| (s, name, path))
                }
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

        if scored.is_empty() {
            *filtered_paths.borrow_mut() = Vec::new();
            let empty = gtk::Label::new(Some("No matching projects"));
            empty.add_css_class("model-picker-empty");
            empty.set_margin_top(16);
            empty.set_margin_bottom(16);
            list.append(&empty);
            return;
        }

        let mut paths = Vec::with_capacity(scored.len());
        let mut row_to_select = None;

        for (rank, (_, name, path)) in scored.into_iter().enumerate() {
            paths.push(path.clone());

            let row = gtk::ListBoxRow::new();
            row.add_css_class("new-session-row");

            let box_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            box_row.set_margin_start(10);
            box_row.set_margin_end(10);
            box_row.set_margin_top(6);
            box_row.set_margin_bottom(6);

            let title_lbl = gtk::Label::new(Some(&name));
            title_lbl.set_xalign(0.0);
            title_lbl.add_css_class("new-session-name");

            let path_lbl = gtk::Label::new(Some(&path));
            path_lbl.set_xalign(1.0);
            path_lbl.set_hexpand(true);
            path_lbl.add_css_class("new-session-path");
            path_lbl.set_ellipsize(pango::EllipsizeMode::Middle);

            box_row.append(&title_lbl);
            box_row.append(&path_lbl);

            row.set_child(Some(&box_row));
            list.append(&row);

            if !query.is_empty() && rank == 0 {
                row_to_select = Some(row.clone());
            }
        }

        if let Some(row) = row_to_select {
            list.select_row(Some(&row));
        } else {
            list.select_row(None::<&gtk::ListBoxRow>);
        }

        *filtered_paths.borrow_mut() = paths;
    }

    fn activate_current_new_session_selection(controller: &Rc<RefCell<Self>>) {
        let (list, filtered_paths) = {
            let this = controller.borrow();
            (
                this.widgets.new_session_list.clone(),
                this.widgets.new_session_filtered_paths.clone(),
            )
        };
        let row_idx = list
            .selected_row()
            .map(|row| row.index() as usize)
            .or_else(|| {
                if list.row_at_index(0).is_some() {
                    Some(0)
                } else {
                    None
                }
            });
        let path = row_idx
            .and_then(|idx| filtered_paths.borrow().get(idx).cloned())
            .or_else(|| filtered_paths.borrow().first().cloned());
        if let Some(path) = path {
            Self::create_session_for_project(controller, &path);
        }
    }

    fn create_session_for_project(controller: &Rc<RefCell<Self>>, directory: &str) {
        let (api, request_id) = {
            let mut this = controller.borrow_mut();
            this.close_new_session_overlay();
            this.next_session_request_id += 1;
            let request_id = this.next_session_request_id;
            this.pending_session_request = Some(request_id);
            (this.api.clone(), request_id)
        };
        api.send(Command::CreateSession {
            request_id,
            directory: directory.to_owned(),
            title: None,
        });
    }

    fn close_new_session_overlay(&self) {
        self.widgets.new_session_overlay.set_visible(false);
        self.widgets.composer.grab_focus();
    }

    fn open_app_modal(&mut self, kind: AppModalKind, width: i32, height: i32) {
        self.close_new_session_overlay();
        clear_box(&self.widgets.app_modal_card);
        self.widgets.app_modal_card.set_sensitive(true);
        self.widgets.app_modal_card.remove_css_class("sessions");
        if kind == AppModalKind::Sessions {
            self.widgets.app_modal_card.add_css_class("sessions");
        }
        self.widgets.app_modal_card.set_hexpand(false);
        self.widgets.app_modal_card.set_vexpand(false);
        self.widgets.app_modal_card.set_size_request(width, height);
        self.widgets.app_modal_overlay.set_visible(true);
        self.app_modal = Some(kind);
        self.app_modal_focus = None;
        self.session_picker = None;
        self.rename_session_id = None;
    }

    fn close_app_modal(&mut self) {
        self.widgets.app_modal_overlay.set_visible(false);
        self.widgets.window.set_default_widget(None::<&gtk::Widget>);
        clear_box(&self.widgets.app_modal_card);
        self.widgets.app_modal_card.remove_css_class("sessions");
        self.widgets.app_modal_card.set_size_request(-1, -1);
        self.app_modal = None;
        self.app_modal_focus = None;
        self.session_picker = None;
        self.rename_session_id = None;
        if self.widgets.composer_stack.visible_child_name().as_deref() == Some("composer") {
            self.widgets.composer.grab_focus();
        }
    }

    fn rename_active_session(controller: &Rc<RefCell<Self>>) {
        let active = controller.borrow().state.active.clone();
        if let Some(active) = active {
            Self::show_rename_session(controller, &active);
        }
    }

    fn show_rename_session(controller: &Rc<RefCell<Self>>, session_id: &str) {
        {
            let this = controller.borrow();
            if this.app_modal == Some(AppModalKind::Rename)
                && this.rename_session_id.as_deref() == Some(session_id)
            {
                this.widgets.app_modal_overlay.set_visible(true);
                if let Some(focus) = &this.app_modal_focus {
                    focus.grab_focus();
                }
                return;
            }
        }
        let Some(session) = controller.borrow().session(session_id).cloned() else {
            return;
        };
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.set_margin_top(18);
        root.set_margin_bottom(18);
        root.set_margin_start(18);
        root.set_margin_end(18);
        let title_label = gtk::Label::new(Some("Session title"));
        title_label.set_xalign(0.0);
        let title = gtk::Entry::new();
        title.set_text(&session.title);
        title.set_activates_default(true);
        title_label.set_mnemonic_widget(Some(&title));
        let id_label = gtk::Label::new(Some("Session ID"));
        id_label.set_xalign(0.0);
        let id_field = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        id_field.add_css_class("session-id-field");
        id_field.set_hexpand(true);
        let id_value = gtk::Label::new(Some(&session.id));
        id_value.set_xalign(0.0);
        id_value.set_hexpand(true);
        id_value.set_selectable(true);
        id_value.set_ellipsize(pango::EllipsizeMode::Middle);
        id_value.add_css_class("session-id-value");
        let copy = copy_text_button(&session.id, "Copy session ID");
        id_field.append(&id_value);
        id_field.append(&copy);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        actions.append(&cancel);
        actions.append(&save);
        root.append(&title_label);
        root.append(&title);
        root.append(&id_label);
        root.append(&id_field);
        root.append(&actions);

        cancel.connect_clicked({
            let weak = Rc::downgrade(controller);
            move |_| {
                if let Some(controller) = weak.upgrade() {
                    controller.borrow_mut().close_app_modal();
                }
            }
        });
        save.connect_clicked({
            let weak = Rc::downgrade(controller);
            let title = title.clone();
            move |_| {
                let value = title.text().trim().to_owned();
                if value.is_empty() {
                    title.add_css_class("error");
                    return;
                }
                if value == session.title {
                    if let Some(controller) = weak.upgrade() {
                        controller.borrow_mut().close_app_modal();
                    }
                    return;
                }
                if let Some(controller) = weak.upgrade() {
                    let command = {
                        let mut this = controller.borrow_mut();
                        this.next_session_request_id += 1;
                        let request_id = this.next_session_request_id;
                        this.pending_rename_request = Some(request_id);
                        Command::RenameSession {
                            request_id,
                            session_id: session.id.clone(),
                            directory: session.directory.clone(),
                            title: value,
                        }
                    };
                    controller.borrow().api.send(command);
                    controller
                        .borrow()
                        .widgets
                        .app_modal_card
                        .set_sensitive(false);
                }
            }
        });
        title.connect_activate({
            let save = save.clone();
            move |_| save.emit_clicked()
        });
        {
            let mut this = controller.borrow_mut();
            this.open_app_modal(AppModalKind::Rename, 350, -1);
            this.widgets.window.set_default_widget(Some(&save));
            this.widgets.app_modal_card.append(&root);
            this.app_modal_focus = Some(title.clone().upcast());
            this.rename_session_id = Some(session_id.to_owned());
        }
        title.select_region(0, -1);
        title.grab_focus();
    }

    fn show_permission(
        controller: &Rc<RefCell<Self>>,
        directory: Option<String>,
        payload: serde_json::Value,
    ) {
        let data = event_data(&payload);
        let Some(request_id) = data
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        else {
            return;
        };
        if controller.borrow().dialogs.contains_key(&request_id) {
            return;
        }
        let session_id = data
            .get("sessionID")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let directory = directory.or_else(|| {
            session_id.as_ref().and_then(|session_id| {
                controller
                    .borrow()
                    .session(session_id)
                    .map(|session| session.directory.clone())
            })
        });
        let Some(directory) = directory else {
            controller
                .borrow_mut()
                .show_error("Permission request did not include a server directory");
            return;
        };
        let request_context = controller
            .borrow()
            .request_context(session_id.as_deref(), &directory);
        let permission = data
            .get("permission")
            .or_else(|| data.get("type"))
            .or_else(|| data.get("title"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("tool action");
        let patterns = match data.get("patterns").or_else(|| data.get("pattern")) {
            Some(serde_json::Value::Array(values)) => values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
            Some(serde_json::Value::String(value)) => value.clone(),
            _ => String::new(),
        };
        let metadata = data
            .get("metadata")
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::to_string_pretty(value).ok())
            .unwrap_or_default();
        let always_patterns = data
            .get("always")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|patterns| !patterns.is_empty());

        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let heading = gtk::Label::new(Some(&format!("Allow {permission}?")));
        heading.set_xalign(0.0);
        heading.add_css_class("prompt-heading");
        root.append(&heading);
        let context = gtk::Label::new(Some(&request_context));
        context.set_xalign(0.0);
        context.set_wrap(true);
        context.set_selectable(true);
        context.add_css_class("session-picker-path");
        root.append(&context);
        let details = gtk::Box::new(gtk::Orientation::Vertical, 12);
        if !patterns.is_empty() {
            let label = gtk::Label::new(Some(&patterns));
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.set_selectable(true);
            label.add_css_class("prompt-detail");
            details.append(&label);
        }
        if !metadata.is_empty() && metadata != "{}" {
            let label = gtk::Label::new(Some(&metadata));
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.set_selectable(true);
            label.set_max_width_chars(90);
            label.add_css_class("prompt-metadata");
            details.append(&label);
        }
        if let Some(patterns) = &always_patterns {
            let heading = gtk::Label::new(Some("Always allow would remember:"));
            heading.set_xalign(0.0);
            heading.add_css_class("question-header");
            details.append(&heading);
            let label = gtk::Label::new(Some(patterns));
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.set_selectable(true);
            label.add_css_class("prompt-detail");
            details.append(&label);
        }
        if details.first_child().is_some() {
            let scroll = gtk::ScrolledWindow::builder()
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .min_content_height(80)
                .max_content_height(320)
                .child(&details)
                .build();
            root.append(&scroll);
        }
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        let reject = gtk::Button::with_label("Deny");
        let once = gtk::Button::with_label("Allow once");
        once.add_css_class("suggested-action");
        actions.append(&reject);
        actions.append(&once);
        let always = always_patterns.map(|_| {
            let button = gtk::Button::with_label("Always allow");
            actions.append(&button);
            button
        });
        root.append(&actions);

        let mut replies = vec![(reject, "reject"), (once, "once")];
        if let Some(always) = always {
            replies.push((always, "always"));
        }
        for (button, reply) in replies {
            let weak = Rc::downgrade(controller);
            let request_id = request_id.clone();
            let directory = directory.clone();
            button.connect_clicked(move |_| {
                submit_request(
                    &weak,
                    &request_id,
                    Command::ReplyPermission {
                        request_id: request_id.clone(),
                        directory: directory.clone(),
                        reply: reply.to_owned(),
                    },
                );
            });
        }
        let widget = root.upcast::<gtk::Widget>();
        let mut this = controller.borrow_mut();
        this.dialogs.insert(request_id.clone(), widget.clone());
        this.composer_prompts.push(ComposerPrompt {
            request_id,
            session_id,
            widget,
        });
        this.refresh_composer_prompt();
    }

    fn show_question(
        controller: &Rc<RefCell<Self>>,
        directory: Option<String>,
        payload: serde_json::Value,
    ) {
        let data = event_data(&payload);
        let Some(request_id) = data
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        else {
            return;
        };
        if controller.borrow().dialogs.contains_key(&request_id) {
            return;
        }
        let session_id = data
            .get("sessionID")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let directory = directory.or_else(|| {
            session_id.as_ref().and_then(|session_id| {
                controller
                    .borrow()
                    .session(session_id)
                    .map(|session| session.directory.clone())
            })
        });
        let Some(directory) = directory else {
            controller
                .borrow_mut()
                .show_error("Question request did not include a server directory");
            return;
        };
        let request_context = controller
            .borrow()
            .request_context(session_id.as_deref(), &directory);
        let questions = data
            .get("questions")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        if questions.is_empty() {
            controller.borrow().api.send(Command::ReplyQuestion {
                request_id,
                directory,
                answers: Vec::new(),
            });
            return;
        }

        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let context = gtk::Label::new(Some(&request_context));
        context.set_xalign(0.0);
        context.set_wrap(true);
        context.set_selectable(true);
        context.add_css_class("session-picker-path");
        root.append(&context);
        let question_box = gtk::Box::new(gtk::Orientation::Vertical, 16);
        let mut inputs = Vec::new();

        for question in questions {
            let section = gtk::Box::new(gtk::Orientation::Vertical, 7);
            section.add_css_class("question-section");
            let header = question
                .get("header")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Question");
            let header = gtk::Label::new(Some(header));
            header.set_xalign(0.0);
            header.add_css_class("question-header");
            section.append(&header);
            let prompt = question
                .get("question")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let prompt = gtk::Label::new(Some(prompt));
            prompt.set_xalign(0.0);
            prompt.set_wrap(true);
            section.append(&prompt);
            let multiple = question
                .get("multiple")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let mut first = None;
            let mut options = Vec::new();
            for option in question
                .get("options")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
            {
                let label = option
                    .get("label")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Option")
                    .to_owned();
                let choice = gtk::CheckButton::with_label(&label);
                if !multiple {
                    if let Some(first) = &first {
                        choice.set_group(Some(first));
                    } else {
                        first = Some(choice.clone());
                    }
                }
                if let Some(description) = option
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .filter(|description| !description.is_empty())
                {
                    choice.set_tooltip_text(Some(description));
                }
                section.append(&choice);
                options.push((choice, label));
            }
            let custom = question
                .get("custom")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true)
                .then(|| {
                    let entry = gtk::Entry::new();
                    entry.set_placeholder_text(Some("Type your own answer"));
                    section.append(&entry);
                    entry
                });
            if !multiple {
                if let Some(entry) = &custom {
                    for (choice, _) in &options {
                        choice.connect_toggled({
                            let entry = entry.clone();
                            move |choice| {
                                if choice.is_active() && !entry.text().is_empty() {
                                    entry.set_text("");
                                }
                            }
                        });
                    }
                    let choices: Vec<_> =
                        options.iter().map(|(choice, _)| choice.clone()).collect();
                    entry.connect_changed(move |entry| {
                        if !entry.text().is_empty() {
                            for choice in &choices {
                                choice.set_active(false);
                            }
                        }
                    });
                }
            }
            question_box.append(&section);
            inputs.push(QuestionInputs {
                options,
                custom,
                multiple,
            });
        }

        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&question_box)
            .build();
        let validation = gtk::Label::new(None);
        validation.set_xalign(0.0);
        validation.add_css_class("error");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        let dismiss = gtk::Button::with_label("Dismiss");
        let submit = gtk::Button::with_label("Submit");
        submit.add_css_class("suggested-action");
        actions.append(&dismiss);
        actions.append(&submit);
        root.append(&scroll);
        root.append(&validation);
        root.append(&actions);

        submit.connect_clicked({
            let weak = Rc::downgrade(controller);
            let request_id = request_id.clone();
            let directory = directory.clone();
            let validation = validation.clone();
            move |_| {
                let answers: Vec<Vec<String>> = inputs
                    .iter()
                    .map(|input| {
                        let mut answer: Vec<_> = input
                            .options
                            .iter()
                            .filter(|(button, _)| button.is_active())
                            .map(|(_, label)| label.clone())
                            .collect();
                        let custom = input
                            .custom
                            .as_ref()
                            .map(|entry| entry.text().trim().to_owned())
                            .filter(|custom| !custom.is_empty());
                        if let Some(custom) = custom {
                            if input.multiple {
                                answer.push(custom);
                            } else {
                                answer = vec![custom];
                            }
                        }
                        answer
                    })
                    .collect();
                if answers.iter().any(Vec::is_empty) {
                    validation.set_label("Choose or enter an answer for every question.");
                    return;
                }
                submit_request(
                    &weak,
                    &request_id,
                    Command::ReplyQuestion {
                        request_id: request_id.clone(),
                        directory: directory.clone(),
                        answers,
                    },
                );
            }
        });
        dismiss.connect_clicked({
            let weak = Rc::downgrade(controller);
            let request_id = request_id.clone();
            let directory = directory.clone();
            move |_| {
                submit_request(
                    &weak,
                    &request_id,
                    Command::RejectQuestion {
                        request_id: request_id.clone(),
                        directory: directory.clone(),
                    },
                );
            }
        });
        let widget = root.upcast::<gtk::Widget>();
        let mut this = controller.borrow_mut();
        this.dialogs.insert(request_id.clone(), widget.clone());
        this.composer_prompts.push(ComposerPrompt {
            request_id,
            session_id,
            widget,
        });
        this.refresh_composer_prompt();
    }

    fn active_directory(&self) -> Option<String> {
        self.state
            .active
            .as_ref()
            .and_then(|active| self.session(active))
            .map(|session| session.directory.clone())
    }

    fn request_context(&self, session_id: Option<&str>, directory: &str) -> String {
        let session = session_id.and_then(|session_id| self.session(session_id));
        let title = session
            .map(|session| session.title.as_str())
            .or(session_id)
            .unwrap_or("Unknown session");
        format!("Requested by {title}\n{directory}")
    }

    fn session(&self, id: &str) -> Option<&Session> {
        self.state.sessions.iter().find(|session| session.id == id)
    }

    fn tab_sessions(&self) -> Vec<Session> {
        self.state
            .tabs
            .iter()
            .map(|id| {
                self.session(id).cloned().unwrap_or_else(|| Session {
                    id: id.clone(),
                    directory: String::new(),
                    title: id.clone(),
                    time: SessionTime {
                        created: 0,
                        updated: 0,
                        archived: None,
                    },
                    parent_id: None,
                    agent: None,
                    model: None,
                })
            })
            .collect()
    }

    fn upsert_session(&mut self, session: Session) {
        if session.time.archived.is_some() {
            if self
                .session(&session.id)
                .is_some_and(|existing| session.time.updated >= existing.time.updated)
            {
                self.remove_session(&session.id);
            }
            return;
        }
        if session.parent_id.is_some() {
            return;
        }
        if let Some(existing) = self
            .state
            .sessions
            .iter_mut()
            .find(|existing| existing.id == session.id)
        {
            if session.time.updated >= existing.time.updated {
                *existing = session;
            }
        } else {
            self.state.sessions.push(session);
        }
        self.state
            .sessions
            .sort_by_key(|session| std::cmp::Reverse(session.time.updated));
    }

    fn remove_session(&mut self, id: &str) {
        self.state.sessions.retain(|session| session.id != id);
        self.state.tabs.retain(|tab| tab != id);
        self.state.conversations.remove(id);
        self.message_events_during_load.remove(id);
        self.replacing_messages.remove(id);
        self.message_reload_pending.remove(id);
        self.state.loading_messages.remove(id);
        self.state.selections.remove(id);
        self.state.statuses.remove(id);
        self.clear_session_unread(id);
        self.state.server_busy.remove(id);
        self.state.abort_requested.remove(id);
        self.state.drafts.remove(id);
        self.state.pending_prompts.remove(id);
        self.state.optimistic_prompts.remove(id);
        self.message_load_errors.remove(id);
        if self.state.active.as_deref() == Some(id) {
            self.state.active = self.state.tabs.last().cloned();
        }
    }

    fn apply_missed_idle(&mut self, statuses_complete: bool) {
        let pending = std::mem::take(&mut self.offline_busy);
        let mut leftover = HashSet::new();
        for id in pending {
            let known = self.state.statuses.get(&id);
            if known.is_some_and(|s| s.is_busy()) {
                leftover.insert(id);
                continue;
            }
            if !statuses_complete && known.is_none() {
                leftover.insert(id);
                continue;
            }
            if self.state.tabs.contains(&id) {
                self.state.unread.insert(id);
            }
        }
        self.offline_busy = leftover;
    }

    fn persist_state(&mut self) {
        if self.persistence_writes_blocked {
            return;
        }
        let tabs = self
            .state
            .tabs
            .iter()
            .filter_map(|id| {
                self.session(id).map(|session| PersistedTab {
                    id: session.id.clone(),
                    directory: session.directory.clone(),
                    title: session.title.clone(),
                })
            })
            .collect();
        self.persisted.servers.insert(
            self.server_key.clone(),
            ServerState {
                tabs,
                active: self.state.active.clone(),
                selections: self.state.selections.clone(),
                unread: self.state.unread.clone(),
                busy: {
                    let mut busy: HashSet<_> = self
                        .state
                        .statuses
                        .iter()
                        .filter(|(_, status)| status.is_busy())
                        .map(|(id, _)| id.clone())
                        .collect();
                    busy.extend(self.offline_busy.iter().cloned());
                    busy
                },
            },
        );
        match self.persisted.save(&self.state_path) {
            Ok(()) => self.persistence_error = None,
            Err(error) => {
                self.persistence_error = Some(error.to_string());
                self.show_error(&error.to_string());
            }
        }
    }

    fn show_error(&mut self, error: &str) {
        self.widgets.status.set_label(error);
        self.widgets.status.add_css_class("error");
    }
}

fn submit_request(
    controller: &Weak<RefCell<Controller>>,
    request_id: &str,
    command: Command,
) -> bool {
    let Some(controller) = controller.upgrade() else {
        return false;
    };
    let api = {
        let mut this = controller.borrow_mut();
        let Some(dialog) = this.dialogs.get(request_id).cloned() else {
            return false;
        };
        if !this.pending_actions.insert(request_id.to_owned()) {
            return true;
        }
        dialog.set_sensitive(false);
        this.api.clone()
    };
    api.send(command);
    true
}

fn sessions_picker_body() -> (gtk::Box, gtk::ListBox, gtk::Entry) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let search = gtk::Entry::new();
    search.set_placeholder_text(Some("Search tabs..."));
    search.set_hexpand(true);
    search.add_css_class("new-session-search");
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("new-session-list");
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(false)
        .propagate_natural_width(false)
        .min_content_width(420)
        .max_content_width(420)
        .min_content_height(264)
        .max_content_height(264)
        .vexpand(true)
        .hexpand(true)
        .child(&list)
        .build();
    root.set_hexpand(true);
    root.set_vexpand(true);
    root.append(&search);
    root.append(&scroll);
    (root, list, search)
}

fn filter_tab_sessions<'a>(sessions: &'a [Session], query: &str) -> Vec<&'a Session> {
    let query = query.trim();
    let mut scored: Vec<(i64, usize, &'a Session)> = sessions
        .iter()
        .enumerate()
        .filter_map(|(idx, session)| {
            if query.is_empty() {
                Some((0, idx, session))
            } else {
                let title_score = fuzzy_score(query, &session.title);
                let dir_score = fuzzy_score(query, &session.directory);
                let score = match (title_score, dir_score) {
                    (Some(s1), Some(s2)) => Some(s1.max(s2)),
                    (s1, s2) => s1.or(s2),
                };
                score.map(|s| (s, idx, session))
            }
        })
        .collect();

    if !query.is_empty() {
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    }

    scored.into_iter().map(|(_, _, session)| session).collect()
}

fn populate_session_list(
    list: &gtk::ListBox,
    sessions: &[Session],
    active_id: Option<&str>,
    query: &str,
    controller: Weak<RefCell<Controller>>,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let filtered = filter_tab_sessions(sessions, query);
    let mut shown = 0;
    for session in filtered {
        if shown == SESSION_PICKER_LIMIT {
            break;
        }
        shown += 1;
        let button = gtk::Button::new();
        button.add_css_class("session-picker-row");
        let is_active = active_id == Some(session.id.as_str());
        if is_active {
            button.add_css_class("active");
        }
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 3);
        let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let title = gtk::Label::new(Some(&session.title));
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_ellipsize(pango::EllipsizeMode::End);
        title.add_css_class("session-picker-title");
        title_box.append(&title);
        if is_active {
            let current = gtk::Label::new(Some("current"));
            current.add_css_class("session-picker-time");
            title_box.append(&current);
        }
        let path = gtk::Label::new(Some(&session.directory));
        path.set_xalign(0.0);
        path.set_hexpand(true);
        path.set_ellipsize(pango::EllipsizeMode::Middle);
        path.add_css_class("session-picker-path");
        let time = gtk::Label::new(Some(&format_local_timestamp(session.time.updated)));
        time.set_xalign(0.0);
        time.add_css_class("session-picker-time");
        labels.append(&title_box);
        labels.append(&path);
        labels.append(&time);
        button.set_child(Some(&labels));
        let id = session.id.clone();
        let weak = controller.clone();
        button.connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::open_tab(&controller, &id);
                controller.borrow_mut().close_app_modal();
            }
        });
        list.append(&button);
    }
    if shown == 0 {
        let empty = gtk::Label::new(Some("No matching tabs"));
        empty.add_css_class("model-picker-empty");
        empty.set_margin_top(16);
        empty.set_margin_bottom(16);
        list.append(&empty);
    }
}

fn filter_all_sessions<'a>(sessions: &'a [Session], query: &str) -> Vec<&'a Session> {
    let query = query.trim();
    let mut scored: Vec<(i64, u64, &'a Session)> = sessions
        .iter()
        .filter_map(|session| {
            if query.is_empty() {
                Some((0, session.time.updated, session))
            } else {
                let title_score = fuzzy_score(query, &session.title);
                let dir_score = fuzzy_score(query, &session.directory);
                let score = match (title_score, dir_score) {
                    (Some(s1), Some(s2)) => Some(s1.max(s2)),
                    (s1, s2) => s1.or(s2),
                };
                score.map(|s| (s, session.time.updated, session))
            }
        })
        .collect();

    scored.sort_by(|a, b| {
        if query.is_empty() {
            b.1.cmp(&a.1)
        } else {
            b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1))
        }
    });

    scored.into_iter().map(|(_, _, session)| session).collect()
}

fn populate_all_sessions_list(
    list: &gtk::ListBox,
    sessions: &[Session],
    open_tabs: &[String],
    query: &str,
    controller: Weak<RefCell<Controller>>,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let filtered = filter_all_sessions(sessions, query);
    let mut shown = 0;
    for session in filtered {
        if shown == SESSION_PICKER_LIMIT {
            break;
        }
        shown += 1;
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.add_css_class("session-picker-row");
        let is_open = open_tabs.iter().any(|id| id == &session.id);
        if is_open {
            button.add_css_class("active");
        }
        let card = gtk::Box::new(gtk::Orientation::Vertical, 5);

        let row_top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let title = gtk::Label::new(Some(&session.title));
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_ellipsize(pango::EllipsizeMode::End);
        title.add_css_class("session-picker-title");
        row_top.append(&title);

        let badge = gtk::Label::new(Some(if is_open { "OPEN TAB" } else { "SERVER" }));
        badge.add_css_class("session-badge");
        badge.add_css_class(if is_open { "open" } else { "server" });
        row_top.append(&badge);

        let row_bottom = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let path = gtk::Label::new(Some(&session.directory));
        path.set_xalign(0.0);
        path.set_hexpand(true);
        path.set_ellipsize(pango::EllipsizeMode::Middle);
        path.add_css_class("session-picker-path");
        row_bottom.append(&path);

        let time = gtk::Label::new(Some(&format_local_timestamp(session.time.updated)));
        time.set_xalign(1.0);
        time.add_css_class("session-picker-time");
        row_bottom.append(&time);

        card.append(&row_top);
        card.append(&row_bottom);
        button.set_child(Some(&card));

        let id = session.id.clone();
        let weak = controller.clone();
        button.connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::open_tab(&controller, &id);
                controller.borrow_mut().close_app_modal();
            }
        });
        list.append(&button);
    }
    if shown == 0 {
        let empty = gtk::Label::new(Some("No matching sessions"));
        empty.add_css_class("model-picker-empty");
        empty.set_margin_top(16);
        empty.set_margin_bottom(16);
        list.append(&empty);
    }
}

fn new_transcript_slot() -> TranscriptVisible {
    let row = transcript_row_widget();
    row.set_hexpand(true);
    row.set_halign(gtk::Align::Fill);
    row.set_valign(gtk::Align::Start);
    row.set_vexpand(false);
    TranscriptVisible {
        index: usize::MAX,
        bound: String::new(),
        row,
    }
}

fn reuse_row_heights(old_rows: &[String], old_heights: &[i32], new_rows: &[String]) -> Vec<i32> {
    if old_rows.is_empty() {
        return vec![ROW_ESTIMATE; new_rows.len()];
    }
    if new_rows.len() > old_rows.len() {
        let inserted = new_rows.len() - old_rows.len();
        if new_rows[inserted..] == *old_rows {
            let mut heights = vec![ROW_ESTIMATE; inserted];
            heights.extend_from_slice(old_heights);
            return heights;
        }
    }
    new_rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            if old_rows.get(index) == Some(row) {
                old_heights.get(index).copied().unwrap_or(ROW_ESTIMATE)
            } else {
                ROW_ESTIMATE
            }
        })
        .collect()
}

fn row_offset(heights: &[i32], index: usize) -> i32 {
    heights.iter().take(index).copied().sum()
}

fn min_visible_end(start: usize, len: usize, page: f64) -> usize {
    let count =
        ((page.max(1.0) as i32 / ROW_MIN_HEIGHT) as usize).saturating_add(ROW_OVERSCAN * 2 + 1);
    start.saturating_add(count).min(len)
}

fn visible_row_range_from_end(heights: &[i32], page: f64, overscan: usize) -> (usize, usize) {
    if heights.is_empty() {
        return (0, 0);
    }
    let end = heights.len();
    let mut y = 0.0;
    let mut start = 0;
    for (index, height) in heights.iter().enumerate().rev() {
        y += f64::from(*height);
        start = index;
        if y >= page.max(1.0) {
            break;
        }
    }
    (start.saturating_sub(overscan), end)
}

fn target_visible_range(
    heights: &[i32],
    scroll: f64,
    page: f64,
    overscan: usize,
    at_bottom: bool,
) -> (usize, usize) {
    let (start, mut end) = if at_bottom {
        visible_row_range_from_end(heights, page, overscan)
    } else {
        visible_row_range(heights, scroll, page, overscan)
    };
    end = end.max(min_visible_end(start, heights.len(), page));
    (start, end)
}

fn distance_from_bottom(adjustment: &gtk::Adjustment) -> f64 {
    adjustment.upper() - (adjustment.value() + adjustment.page_size())
}

fn visible_row_range(heights: &[i32], scroll: f64, page: f64, overscan: usize) -> (usize, usize) {
    if heights.is_empty() {
        return (0, 0);
    }
    let top = scroll.max(0.0);
    let bottom = top + page.max(1.0);
    let mut y = 0.0;
    let mut start = 0;
    let mut end = heights.len();
    for (index, height) in heights.iter().enumerate() {
        let next = y + f64::from(*height);
        if next > top {
            start = index;
            break;
        }
        y = next;
    }
    y = 0.0;
    for (index, height) in heights.iter().enumerate() {
        y += f64::from(*height);
        if y >= bottom {
            end = index + 1;
            break;
        }
    }
    (
        start.saturating_sub(overscan),
        (end + overscan).min(heights.len()),
    )
}

fn transcript_session_changed(
    rendered_session: Option<&str>,
    active_session: Option<&str>,
) -> bool {
    rendered_session != active_session
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn transcript_row_is_user(row: &str) -> bool {
    serde_json::from_str::<TranscriptRow>(row).is_ok_and(|row| row.role == "YOU")
}

fn apply_optimistic_row(
    mut rows: Vec<String>,
    optimistic: Option<&OptimisticPrompt>,
) -> (Vec<String>, bool) {
    let Some(optimistic) = optimistic else {
        return (rows, false);
    };
    let you = rows
        .iter()
        .filter(|row| transcript_row_is_user(row))
        .count();
    if you > optimistic.baseline_you {
        (rows, true)
    } else {
        rows.push(optimistic.row.clone());
        (rows, false)
    }
}

fn optimistic_transcript_row(draft: &Draft, created: u64) -> String {
    let mut blocks = Vec::new();
    let mut images = Vec::new();
    if !draft.text.is_empty() {
        blocks.push(draft.text.clone());
    }
    for path in &draft.attachments {
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attachment");
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        blocks.push(format!("Attached: {filename} ({mime})"));
        if mime.as_ref().starts_with("image/") {
            if let Some(url) = attachment_data_url(path, mime.as_ref()) {
                images.push(url);
            }
        }
    }
    serde_json::json!({
        "role": "YOU",
        "body": blocks.join("\n\n"),
        "images": images,
        "time": created,
        "kind": "",
    })
    .to_string()
}

fn attachment_data_url(path: &PathBuf, mime: &str) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    (bytes.len() <= MAX_INLINE_IMAGE_BYTES)
        .then(|| format!("data:{mime};base64,{}", BASE64.encode(bytes)))
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RealizedRowBounds {
    index: usize,
    top: f64,
    bottom: f64,
    user: bool,
}

fn row_layout_bounds(rows: &[String], heights: &[i32]) -> Vec<RealizedRowBounds> {
    let mut y = 0.0;
    rows.iter()
        .zip(heights)
        .enumerate()
        .map(|(index, (row, height))| {
            let top = y;
            y += f64::from(*height);
            RealizedRowBounds {
                index,
                top,
                bottom: y,
                user: transcript_row_is_user(row),
            }
        })
        .collect()
}

fn sticky_user_index(
    user_indices: &[usize],
    realized: &[RealizedRowBounds],
    viewport_top: f64,
    viewport_bottom: f64,
) -> Option<usize> {
    let first_realized = realized.iter().map(|row| row.index).min();
    let last_realized = realized.iter().map(|row| row.index).max();
    let realized_users: HashMap<usize, (f64, f64)> = realized
        .iter()
        .filter(|row| row.user)
        .map(|row| (row.index, (row.top, row.bottom)))
        .collect();

    let mut last_past_top = None;
    let mut user_flush_with_top = false;
    for &index in user_indices {
        let (top, bottom) = if let Some(&bounds) = realized_users.get(&index) {
            bounds
        } else if first_realized.is_some_and(|first| index < first) {
            (f64::NEG_INFINITY, f64::NEG_INFINITY)
        } else if last_realized.is_some_and(|last| index > last) {
            (f64::INFINITY, f64::INFINITY)
        } else {
            continue;
        };
        if top < viewport_top - BOTTOM_EPSILON {
            last_past_top = Some(index);
            continue;
        }
        if top > viewport_bottom - BOTTOM_EPSILON {
            continue;
        }
        let fully_visible = bottom <= viewport_bottom + BOTTOM_EPSILON;
        if top <= viewport_top + BOTTOM_EPSILON && fully_visible {
            user_flush_with_top = true;
        }
    }
    if user_flush_with_top {
        None
    } else {
        last_past_top
    }
}

fn sticky_message_text(row: TranscriptRow) -> String {
    if !row.body.is_empty() || row.images.is_empty() {
        row.body
    } else if row.images.len() == 1 {
        "Attached image".to_owned()
    } else {
        format!("Attached {} images", row.images.len())
    }
}

fn resolved_variant_index(
    saved: Option<&Option<String>>,
    current_variants: &[Option<String>],
    model_loaded: bool,
) -> (usize, bool) {
    if let Some(saved) = saved {
        if let Some(index) = current_variants.iter().position(|variant| variant == saved) {
            return (index, false);
        }
        if model_loaded && saved.is_some() {
            return (0, true);
        }
    }
    (0, false)
}

fn project_paths(projects: &[Project], sessions: &[Session]) -> Vec<String> {
    let mut paths = Vec::new();
    for project in projects {
        push_unique_path(&mut paths, project.worktree.clone());
    }
    for session in sessions {
        push_unique_path(&mut paths, session.directory.clone());
    }
    paths
}

fn push_unique_path(paths: &mut Vec<String>, path: String) {
    if !path.is_empty() && !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn format_local_timestamp(timestamp: u64) -> String {
    let seconds = if timestamp >= 1_000_000_000_000 {
        (timestamp / 1000) as i64
    } else {
        timestamp as i64
    };
    glib::DateTime::from_unix_utc(seconds)
        .ok()
        .and_then(|utc| utc.to_local().ok())
        .and_then(|local| local.format("%Y-%m-%d %H:%M").ok())
        .map(|formatted| formatted.to_string())
        .unwrap_or_default()
}

fn display_local_timestamp(timestamp: u64) -> Option<String> {
    let seconds = if timestamp >= 1_000_000_000_000 {
        timestamp / 1000
    } else {
        timestamp
    };
    (seconds >= 1_000_000_000)
        .then(|| format_local_timestamp(timestamp))
        .filter(|formatted| !formatted.is_empty())
}

#[cfg(test)]
fn context_usage_fits(available: i32, natural: i32, visible: bool) -> bool {
    if natural <= 0 || available <= 0 {
        return false;
    }
    available >= natural + if visible { 8 } else { 32 }
}

fn apply_tab_shortcut_hint(bar: &gtk::Box, held: bool) {
    let mut child = bar.first_child();
    while let Some(tab) = child {
        let next = tab.next_sibling();
        if tab.has_css_class("session-tab") {
            if let Some(slot) = tab
                .first_child()
                .and_then(|widget| widget.downcast::<gtk::Box>().ok())
            {
                if slot.has_css_class("session-tab-hint") {
                    let status = slot.first_child();
                    let number = status.as_ref().and_then(gtk::Widget::next_sibling);
                    let show_index = held && number.is_some();
                    if let Some(status) = status {
                        status.set_visible(!show_index);
                        if !show_index {
                            if let Ok(spinner) = status.downcast::<gtk::Spinner>() {
                                spinner.start();
                            }
                        }
                    }
                    if let Some(number) = number {
                        number.set_visible(show_index);
                    }
                }
            }
        }
        child = next;
    }
}

fn pointer_hits_widget(
    widget: &impl IsA<gtk::Widget>,
    origin: &impl IsA<gtk::Widget>,
    x: f64,
    y: f64,
) -> bool {
    if !widget.is_visible() || widget.parent().is_none() {
        return false;
    }
    origin
        .pick(x, y, gtk::PickFlags::DEFAULT)
        .is_some_and(|picked| is_self_or_descendant(widget, &picked))
}

fn is_self_or_descendant(root: &impl IsA<gtk::Widget>, widget: &gtk::Widget) -> bool {
    let root = root.upcast_ref::<gtk::Widget>();
    let mut current = Some(widget.clone());
    while let Some(node) = current {
        if node == *root {
            return true;
        }
        current = node.parent();
    }
    false
}

fn adjustment_at_top(adjustment: &gtk::Adjustment) -> bool {
    viewport_at_top(adjustment.value(), adjustment.lower())
}

fn adjustment_at_bottom(adjustment: &gtk::Adjustment) -> bool {
    viewport_at_bottom(
        adjustment.value(),
        adjustment.page_size(),
        adjustment.upper(),
    )
}

fn viewport_at_bottom(value: f64, page_size: f64, upper: f64) -> bool {
    value + page_size >= upper - BOTTOM_EPSILON
}

fn viewport_at_top(value: f64, lower: f64) -> bool {
    value <= lower + BOTTOM_EPSILON
}

fn apply_user_bottom_pin(pinned: bool, at_bottom: bool, scrolled_up: bool) -> (bool, bool) {
    if at_bottom {
        (true, false)
    } else if pinned && scrolled_up {
        (false, true)
    } else {
        (pinned, false)
    }
}

fn clamp_adjustment(adjustment: &gtk::Adjustment, value: f64) -> f64 {
    let lower = adjustment.lower();
    let upper = (adjustment.upper() - adjustment.page_size()).max(lower);
    value.clamp(lower, upper)
}

fn scroll_adjustment_by(adjustment: &gtk::Adjustment, delta: f64) -> bool {
    let value = adjustment.value();
    let Some(target) = scroll_target(
        value,
        adjustment.lower(),
        adjustment.upper(),
        adjustment.page_size(),
        adjustment.step_increment(),
        delta,
    ) else {
        return false;
    };
    adjustment.set_value(target);
    true
}

fn scroll_target(
    value: f64,
    lower: f64,
    upper: f64,
    page_size: f64,
    step_increment: f64,
    delta: f64,
) -> Option<f64> {
    let upper = (upper - page_size).max(lower);
    let target = (value + delta * step_increment.max(24.0)).clamp(lower, upper);
    ((target - value).abs() >= f64::EPSILON).then_some(target)
}

fn scroll_adjustment_to_bottom(adjustment: &gtk::Adjustment) {
    let bottom = adjustment.upper() - adjustment.page_size();
    let target = clamp_adjustment(adjustment, bottom);
    if (adjustment.value() - target).abs() > BOTTOM_EPSILON {
        if debug_logging_enabled() {
            debug_log(format!(
                "pin-bottom from={:.1} to={:.1} page={:.1} upper={:.1}",
                adjustment.value(),
                target,
                adjustment.page_size(),
                adjustment.upper()
            ));
        }
        adjustment.set_value(target);
    }
}

fn transcript_indicator(
    has_session: bool,
    loading: bool,
    replacing: bool,
    loaded: bool,
    has_rows: bool,
    working: bool,
    load_error: bool,
) -> TranscriptIndicator {
    if !has_session {
        TranscriptIndicator::NoSession
    } else if loading && replacing && has_rows {
        TranscriptIndicator::Refreshing
    } else if loading && !has_rows {
        TranscriptIndicator::Loading
    } else if load_error {
        TranscriptIndicator::Error
    } else if !loaded && !has_rows {
        TranscriptIndicator::Loading
    } else if !has_rows {
        TranscriptIndicator::Empty
    } else if working {
        TranscriptIndicator::Working
    } else {
        TranscriptIndicator::Hidden
    }
}

#[cfg(test)]
fn event_returns_control(payload: &serde_json::Value) -> bool {
    payload.get("type").and_then(serde_json::Value::as_str) == Some("session.idle")
}

fn session_idle_marks_unread(previous: Option<&RunStatus>) -> bool {
    previous.is_some_and(|s| s.is_busy())
}

fn session_notification_id(server_key: &str, session_id: &str) -> String {
    format!("{server_key}:session:{session_id}")
}

fn should_follow_transcript(update: TranscriptUpdate, at_bottom: bool) -> bool {
    update == TranscriptUpdate::Activate || at_bottom
}

fn is_visible_session_tab(widget: &gtk::Widget) -> bool {
    widget.has_css_class("session-tab") && widget.is_visible()
}

fn visible_tab_ids(bar: &gtk::Box) -> Vec<String> {
    let mut ids = Vec::new();
    let mut child = bar.first_child();
    while let Some(widget) = child {
        if is_visible_session_tab(&widget) {
            let name = widget.widget_name();
            if !name.is_empty() {
                ids.push(name.to_string());
            }
        }
        child = widget.next_sibling();
    }
    ids
}

fn visible_tab_index(tab: &impl IsA<gtk::Widget>) -> Option<usize> {
    let tab = tab.upcast_ref::<gtk::Widget>();
    let parent = tab.parent()?;
    let mut index = 0;
    let mut child = parent.first_child();
    while let Some(widget) = child {
        if &widget == tab {
            return Some(index);
        }
        if is_visible_session_tab(&widget) {
            index += 1;
        }
        child = widget.next_sibling();
    }
    None
}

fn insert_index_for_target(target: &gtk::Widget, after: bool) -> usize {
    let index = visible_tab_index(target).unwrap_or(0);
    if after {
        index + 1
    } else {
        index
    }
}

const TAB_SLOT_MS: u64 = 140;

fn tab_drop_slot(weak: &Weak<RefCell<Controller>>) -> gtk::Revealer {
    let child = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    child.add_css_class("tab-drop-slot");
    child.set_hexpand(true);
    child.set_size_request(-1, 36);
    let slot = gtk::Revealer::new();
    slot.add_css_class("tab-drop-slot");
    slot.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    slot.set_transition_duration(TAB_SLOT_MS as u32);
    slot.set_child(Some(&child));
    let drop_target = gtk::DropTarget::new(String::static_type(), gdk::DragAction::MOVE);
    let weak_drop = weak.clone();
    drop_target.connect_drop(move |_, value, _, _| {
        let Ok(source_id) = value.get::<String>() else {
            return false;
        };
        let Some(controller) = weak_drop.upgrade() else {
            return false;
        };
        Controller::commit_tab_order(&controller, &source_id)
    });
    drop_target.connect_motion(|_, _, _| gdk::DragAction::MOVE);
    slot.add_controller(drop_target);
    slot
}

fn insert_slot_at(bar: &gtk::Box, index: usize, slot: &gtk::Revealer) {
    if slot.parent().is_some() {
        bar.remove(slot);
    }
    let mut tab_index = 0;
    let mut child = bar.first_child();
    while let Some(widget) = child {
        if is_visible_session_tab(&widget) {
            if tab_index == index {
                match widget.prev_sibling() {
                    Some(sibling) => bar.insert_child_after(slot, Some(&sibling)),
                    None => bar.insert_child_after(slot, None::<&gtk::Widget>),
                }
                return;
            }
            tab_index += 1;
        }
        child = widget.next_sibling();
    }
    bar.append(slot);
}

fn open_slot_at(bar: &gtk::Box, dnd: &Rc<RefCell<TabDnd>>, gen: u64) {
    let (slot, index) = {
        let dnd = dnd.borrow();
        if dnd.gen != gen {
            return;
        }
        match (dnd.slot.clone(), dnd.index) {
            (Some(slot), Some(index)) => (slot, index),
            _ => return,
        }
    };
    insert_slot_at(bar, index, &slot);
    slot.set_reveal_child(true);
}

fn place_drop_slot(
    bar: &gtk::Box,
    index: usize,
    dnd: &Rc<RefCell<TabDnd>>,
    weak: &Weak<RefCell<Controller>>,
) {
    bar.add_css_class("reordering");
    let mut dnd_mut = dnd.borrow_mut();
    if dnd_mut.index == Some(index) && dnd_mut.slot.is_some() {
        return;
    }
    dnd_mut.index = Some(index);
    if let Some(slot) = dnd_mut.slot.clone() {
        let moving = slot.reveals_child();
        let gen = dnd_mut.gen;
        drop(dnd_mut);
        if moving {
            slot.set_reveal_child(false);
            let bar = bar.clone();
            let dnd = dnd.clone();
            glib::timeout_add_local_once(Duration::from_millis(TAB_SLOT_MS), move || {
                open_slot_at(&bar, &dnd, gen);
            });
        }
        return;
    }
    let slot = tab_drop_slot(weak);
    slot.set_transition_duration(0);
    slot.set_reveal_child(true);
    insert_slot_at(bar, index, &slot);
    slot.set_transition_duration(TAB_SLOT_MS as u32);
    dnd_mut.slot = Some(slot);
}

fn tab_drag_preview(title: &str, active: bool) -> gtk::Box {
    let preview = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    preview.add_css_class("session-tab");
    preview.add_css_class("drag-preview");
    if active {
        preview.add_css_class("active");
    }
    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.set_max_width_chars(24);
    label.add_css_class("session-tab-title");
    preview.append(&label);
    preview
}

#[cfg_attr(not(test), allow(dead_code))]
fn move_tab(tabs: &mut Vec<String>, source_id: &str, target_id: &str, after: bool) -> bool {
    if source_id == target_id {
        return false;
    }
    let Some(source_index) = tabs.iter().position(|id| id == source_id) else {
        return false;
    };
    let Some(target_index) = tabs.iter().position(|id| id == target_id) else {
        return false;
    };
    let source = tabs.remove(source_index);
    let target_index = if source_index < target_index {
        target_index - 1
    } else {
        target_index
    };
    let destination = target_index + usize::from(after);
    if destination == source_index {
        tabs.insert(source_index, source);
        return false;
    }
    tabs.insert(destination, source);
    true
}

fn play_session_ready_sound() {
    std::thread::spawn(|| {
        if let Ok(mut child) = std::process::Command::new("canberra-gtk-play")
            .args(["-i", "message-new-instant", "-d", "Session ready"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            let _ = child.wait();
        }
    });
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        clear_selectable_text(&child);
        container.remove(&child);
    }
}

fn prepare_transcript_row_for_recycle(
    window: &gtk::ApplicationWindow,
    row: &impl IsA<gtk::Widget>,
) {
    let row = row.upcast_ref::<gtk::Widget>();
    if let Some(focus) = gtk::prelude::RootExt::focus(window) {
        if &focus == row || focus.is_ancestor(row) {
            gtk::prelude::GtkWindowExt::set_focus(window, None::<&gtk::Widget>);
        }
    }
    clear_selectable_text(row);
}

fn clear_selectable_text(widget: &gtk::Widget) {
    if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
        if label.is_selectable() {
            label.select_region(0, 0);
            label.set_selectable(false);
        }
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        let next = current.next_sibling();
        clear_selectable_text(&current);
        child = next;
    }
}

fn buffer_text(buffer: &gtk::TextBuffer) -> String {
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string()
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    let Some(settings) = gtk::Settings::default() else {
        provider.load_from_data(include_str!("style.css"));
        return;
    };
    settings.set_gtk_error_bell(false);
    settings.set_gtk_xft_antialias(1);
    settings.set_gtk_xft_hinting(1);
    settings.set_gtk_xft_hintstyle(Some("hintslight"));
    settings.set_gtk_xft_rgba(Some("rgb"));
    let mode = Rc::new(Cell::new(
        dark_light::detect().unwrap_or(dark_light::Mode::Unspecified),
    ));
    apply_system_theme(&provider, &settings, mode.get());
    for property in ["gtk-theme-name", "gtk-application-prefer-dark-theme"] {
        let provider = provider.clone();
        let mode = mode.clone();
        settings.connect_notify_local(Some(property), move |settings, _| {
            apply_system_theme(&provider, settings, mode.get());
        });
    }

    let (sender, receiver) = async_channel::unbounded();
    std::thread::spawn(move || {
        let Ok(watcher) = dark_light::subscribe() else {
            return;
        };
        for mode in watcher.iter() {
            if sender.send_blocking(mode).is_err() {
                break;
            }
        }
    });
    glib::spawn_future_local(async move {
        while let Ok(next_mode) = receiver.recv().await {
            mode.set(next_mode);
            apply_system_theme(&provider, &settings, next_mode);
        }
    });
}

fn apply_system_theme(
    provider: &gtk::CssProvider,
    settings: &gtk::Settings,
    mode: dark_light::Mode,
) {
    let theme_name = settings.property::<String>("gtk-theme-name");
    let prefer_dark = settings.property::<bool>("gtk-application-prefer-dark-theme");
    provider.load_from_data(if system_theme_is_dark(mode, &theme_name, prefer_dark) {
        include_str!("style.css")
    } else {
        concat!(
            include_str!("style.css"),
            "\n",
            include_str!("style-light.css")
        )
    });
}

fn system_theme_is_dark(mode: dark_light::Mode, theme_name: &str, prefer_dark: bool) -> bool {
    match mode {
        dark_light::Mode::Dark => true,
        dark_light::Mode::Light => false,
        dark_light::Mode::Unspecified => {
            prefer_dark || theme_name.to_ascii_lowercase().contains("dark")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gtk_debug_init() {
        std::env::set_var("GTK_A11Y", "none");
        std::env::set_var("GSK_RENDERER", "cairo");
        assert!(
            gtk::init().is_ok(),
            "gtk::init failed; run with DISPLAY (Xvfb :99) and cargo test -- --ignored"
        );
        let provider = gtk::CssProvider::new();
        provider.load_from_data(include_str!("style.css"));
        let display = gdk::Display::default().expect("gdk display");
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    fn pump_until_allocated(widget: &impl IsA<gtk::Widget>) {
        let context = glib::MainContext::default();
        for _ in 0..200 {
            while context.iteration(false) {}
            if widget.width() > 0 && widget.height() > 0 {
                return;
            }
            context.iteration(true);
        }
        panic!(
            "widget never allocated ({}x{})",
            widget.width(),
            widget.height()
        );
    }

    fn sessions_overlay_card() -> (gtk::Window, gtk::Box, gtk::ListBox) {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("app-modal-palette");
        card.add_css_class("sessions");
        card.set_overflow(gtk::Overflow::Hidden);
        card.set_hexpand(false);
        card.set_vexpand(false);
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(420, 310);
        let (root, list, _) = sessions_picker_body();
        card.append(&root);
        let window = gtk::Window::new();
        window.set_decorated(false);
        window.set_default_size(800, 600);
        window.set_child(Some(&card));
        (window, card, list)
    }

    fn fill_session_rows(list: &gtk::ListBox, titles: &[&str]) {
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        for title_text in titles {
            let button = gtk::Button::new();
            button.add_css_class("session-picker-row");
            let labels = gtk::Box::new(gtk::Orientation::Vertical, 3);
            let title = gtk::Label::new(Some(title_text));
            title.set_xalign(0.0);
            title.set_hexpand(true);
            title.set_ellipsize(pango::EllipsizeMode::End);
            title.add_css_class("session-picker-title");
            let path = gtk::Label::new(Some("/Users/danny/.opencode-root-project"));
            path.set_xalign(0.0);
            path.set_hexpand(true);
            path.set_ellipsize(pango::EllipsizeMode::Middle);
            path.add_css_class("session-picker-path");
            let time = gtk::Label::new(Some("2026-09-07 13:16"));
            time.set_xalign(0.0);
            time.add_css_class("session-picker-time");
            labels.append(&title);
            labels.append(&path);
            labels.append(&time);
            button.set_child(Some(&labels));
            list.append(&button);
        }
    }

    #[test]
    #[ignore]
    fn debug_sessions_overlay_allocated_size() {
        gtk_debug_init();
        let (window, card, list) = sessions_overlay_card();
        fill_session_rows(
            &list,
            &[
                "opencode-gtk 2",
                "Model Cache Efficiency Review",
                "Performance - resize",
            ],
        );
        window.present();
        pump_until_allocated(&card);
        let wide = (card.width(), card.height());
        fill_session_rows(&list, &["reflection rescue", "reflection dev", "refle"]);
        card.queue_resize();
        pump_until_allocated(&card);
        let narrow = (card.width(), card.height());
        window.close();
        eprintln!("sessions overlay allocated long titles {wide:?} short titles {narrow:?}");
        assert_eq!(
            wide, narrow,
            "allocated size changed with title length: {wide:?} vs {narrow:?}"
        );
        assert_eq!(wide.0, 420);
        assert!(
            wide.1 >= 310,
            "allocated height {} is shorter than 310",
            wide.1
        );
    }

    fn config_with_cloudflare() -> ApiConfig {
        ApiConfig {
            base_url: "https://opencode.example.com".into(),
            username: "opencode".into(),
            password: None,
            cloudflare_access: Some(
                CloudflareAccessCredentials::new("client.access".into(), "secret".into()).unwrap(),
            ),
        }
    }

    #[test]
    fn number_keys_map_to_tab_indexes() {
        assert_eq!(tab_index_from_key(gdk::Key::_1), Some(0));
        assert_eq!(tab_index_from_key(gdk::Key::_9), Some(8));
        assert_eq!(tab_index_from_key(gdk::Key::KP_3), Some(2));
        assert_eq!(tab_index_from_key(gdk::Key::_0), None);
    }

    #[test]
    fn switching_sessions_requires_a_full_transcript_replacement() {
        assert!(transcript_session_changed(Some("ses_one"), Some("ses_two")));
        assert!(!transcript_session_changed(
            Some("ses_one"),
            Some("ses_one")
        ));
    }

    #[test]
    fn bottom_tracking_uses_a_small_layout_tolerance() {
        assert!(viewport_at_bottom(700.0, 300.0, 1000.0));
        assert!(viewport_at_bottom(698.5, 300.0, 1000.0));
        assert!(!viewport_at_bottom(697.0, 300.0, 1000.0));
    }

    #[test]
    fn top_tracking_uses_a_small_layout_tolerance() {
        assert!(viewport_at_top(0.0, 0.0));
        assert!(viewport_at_top(1.5, 0.0));
        assert!(!viewport_at_top(3.0, 0.0));
    }

    #[test]
    fn sticky_user_index_follows_the_latest_scrolled_prompt() {
        let users = [0, 2];
        let realized = [
            RealizedRowBounds {
                index: 0,
                top: 0.0,
                bottom: 40.0,
                user: true,
            },
            RealizedRowBounds {
                index: 1,
                top: 40.0,
                bottom: 80.0,
                user: false,
            },
            RealizedRowBounds {
                index: 2,
                top: 80.0,
                bottom: 120.0,
                user: true,
            },
            RealizedRowBounds {
                index: 3,
                top: 120.0,
                bottom: 400.0,
                user: false,
            },
        ];
        assert_eq!(sticky_user_index(&users, &realized, 200.0, 500.0), Some(2));
        assert_eq!(sticky_user_index(&users, &realized, 50.0, 70.0), Some(0));
        assert_eq!(sticky_user_index(&users, &realized, 50.0, 100.0), Some(0));
        assert_eq!(sticky_user_index(&users, &realized, 80.0, 400.0), None);
        assert_eq!(sticky_user_index(&users, &realized, 90.0, 400.0), Some(2));
        assert_eq!(
            sticky_user_index(
                &[0, 2],
                &[
                    RealizedRowBounds {
                        index: 0,
                        top: -24.0,
                        bottom: 40.0,
                        user: true,
                    },
                    RealizedRowBounds {
                        index: 2,
                        top: 80.0,
                        bottom: 140.0,
                        user: true,
                    },
                ],
                0.0,
                640.0
            ),
            Some(0)
        );
        assert_eq!(
            sticky_user_index(
                &[0],
                &[RealizedRowBounds {
                    index: 0,
                    top: 12.0,
                    bottom: 96.0,
                    user: true,
                }],
                0.0,
                640.0
            ),
            None
        );
    }

    #[test]
    fn sticky_user_index_uses_unrealized_rows_above_the_viewport() {
        let users = [0, 4];
        let realized = [RealizedRowBounds {
            index: 5,
            top: 20.0,
            bottom: 80.0,
            user: false,
        }];
        assert_eq!(sticky_user_index(&users, &realized, 20.0, 300.0), Some(4));
    }

    #[test]
    fn image_only_prompts_have_sticky_text() {
        assert_eq!(
            sticky_message_text(TranscriptRow {
                role: "YOU".to_owned(),
                body: String::new(),
                images: vec!["data:image/png;base64,AA==".to_owned()],
                time: 0,
                kind: String::new(),
            }),
            "Attached image"
        );
    }

    #[test]
    fn variant_index_keeps_saved_effort_until_the_model_is_loaded() {
        let variants = [
            None,
            Some("low".into()),
            Some("medium".into()),
            Some("high".into()),
        ];
        let high = Some("high".into());
        assert_eq!(
            resolved_variant_index(Some(&high), &[None], false),
            (0, false)
        );
        assert_eq!(
            resolved_variant_index(Some(&high), &variants, true),
            (3, false)
        );
        assert_eq!(
            resolved_variant_index(Some(&high), &[None, Some("low".into())], true),
            (0, true)
        );
        assert_eq!(
            resolved_variant_index(Some(&None), &variants, true),
            (0, false)
        );
    }

    #[test]
    fn context_usage_hides_until_there_is_room() {
        assert!(!context_usage_fits(0, 72, false));
        assert!(!context_usage_fits(90, 72, false));
        assert!(context_usage_fits(104, 72, false));
        assert!(context_usage_fits(80, 72, true));
        assert!(!context_usage_fits(79, 72, true));
    }

    #[test]
    fn project_paths_include_session_directories() {
        assert_eq!(
            project_paths(
                &[Project {
                    worktree: "/repo".into(),
                    name: None,
                }],
                &[Session {
                    id: "ses_1".into(),
                    directory: "/Users/danny/.opencode-root-project".into(),
                    title: "Catch all".into(),
                    time: SessionTime {
                        created: 1,
                        updated: 2,
                        archived: None,
                    },
                    parent_id: None,
                    agent: None,
                    model: None,
                }]
            ),
            [
                "/repo".to_string(),
                "/Users/danny/.opencode-root-project".to_string()
            ]
        );
    }

    #[test]
    fn local_timestamps_use_the_host_timezone() {
        let formatted = format_local_timestamp(1_704_067_200_000);
        assert!(formatted.chars().any(|ch| ch.is_ascii_digit()));
        assert!(formatted.contains('-'));
        assert!(formatted.contains(':'));
        assert_eq!(display_local_timestamp(1), None);
        assert!(display_local_timestamp(1_704_067_200_000).is_some());
    }

    #[test]
    fn transcript_indicator_distinguishes_loading_refreshing_and_working() {
        assert_eq!(
            transcript_indicator(true, true, true, false, false, false, false),
            TranscriptIndicator::Loading
        );
        assert_eq!(
            transcript_indicator(true, true, true, true, true, false, false),
            TranscriptIndicator::Refreshing
        );
        assert_eq!(
            transcript_indicator(true, false, false, true, true, true, false),
            TranscriptIndicator::Working
        );
        assert_eq!(
            transcript_indicator(true, false, false, true, true, false, false),
            TranscriptIndicator::Hidden
        );
    }

    #[test]
    fn notifications_only_follow_the_terminal_idle_event() {
        assert!(event_returns_control(&serde_json::json!({
            "type": "session.idle"
        })));
        for event_type in [
            "session.status",
            "session.execution.succeeded",
            "session.error",
        ] {
            assert!(!event_returns_control(&serde_json::json!({
                "type": event_type
            })));
        }
    }

    #[test]
    fn unread_requires_a_busy_to_idle_transition() {
        assert!(session_idle_marks_unread(Some(&RunStatus::Busy)));
        assert!(session_idle_marks_unread(Some(&RunStatus::Retry {
            message: "retry".into(),
            attempt: 1,
        })));
        assert!(!session_idle_marks_unread(Some(&RunStatus::Idle)));
        assert!(!session_idle_marks_unread(None));
    }

    #[test]
    fn notification_ids_are_scoped_to_the_server() {
        assert_ne!(
            session_notification_id("https://one.example", "ses_same"),
            session_notification_id("https://two.example", "ses_same")
        );
    }

    #[test]
    fn sticky_scroll_can_fall_through_at_its_bounds() {
        assert_eq!(scroll_target(0.0, 0.0, 100.0, 40.0, 10.0, 1.0), Some(24.0));
        assert_eq!(scroll_target(60.0, 0.0, 100.0, 40.0, 10.0, 1.0), None);
        assert_eq!(
            scroll_target(60.0, 0.0, 100.0, 40.0, 10.0, -1.0),
            Some(36.0)
        );
    }

    #[test]
    fn system_theme_tracks_dark_variants_and_explicit_preference() {
        assert!(system_theme_is_dark(
            dark_light::Mode::Dark,
            "Adwaita",
            false
        ));
        assert!(!system_theme_is_dark(
            dark_light::Mode::Light,
            "Adwaita-dark",
            true
        ));
        assert!(system_theme_is_dark(
            dark_light::Mode::Unspecified,
            "Adwaita-dark",
            false
        ));
        assert!(system_theme_is_dark(
            dark_light::Mode::Unspecified,
            "Adwaita",
            true
        ));
        assert!(!system_theme_is_dark(
            dark_light::Mode::Unspecified,
            "Adwaita",
            false
        ));
    }

    #[test]
    fn activating_a_tab_forces_the_transcript_to_follow_the_latest_message() {
        assert!(should_follow_transcript(TranscriptUpdate::Activate, false));
        assert!(should_follow_transcript(TranscriptUpdate::Content, true));
        assert!(!should_follow_transcript(TranscriptUpdate::Content, false));
    }

    #[test]
    fn content_growth_keeps_a_bottom_pin() {
        assert_eq!(apply_user_bottom_pin(true, true, false), (true, false));
        assert_eq!(apply_user_bottom_pin(false, true, false), (true, false));
        assert_eq!(apply_user_bottom_pin(true, false, false), (true, false));
        assert_eq!(apply_user_bottom_pin(false, false, false), (false, false));
    }

    #[test]
    fn scrolling_up_clears_a_bottom_pin() {
        assert_eq!(apply_user_bottom_pin(true, false, true), (false, true));
        assert_eq!(apply_user_bottom_pin(false, false, true), (false, false));
    }

    #[test]
    fn visible_range_covers_the_viewport_plus_overscan() {
        let heights = [80, 80, 80, 80, 80, 80];
        assert_eq!(visible_row_range(&heights, 0.0, 100.0, 1), (0, 3));
        assert_eq!(visible_row_range(&heights, 160.0, 80.0, 1), (1, 4));
        assert_eq!(visible_row_range(&[], 0.0, 100.0, 1), (0, 0));
    }

    #[test]
    fn pinned_range_starts_from_the_end() {
        let heights = [80, 80, 80, 80, 80, 80];
        assert_eq!(visible_row_range_from_end(&heights, 100.0, 1), (3, 6));
        assert_eq!(visible_row_range_from_end(&[], 100.0, 1), (0, 0));
    }

    #[test]
    fn target_visible_range_when_pinned_at_bottom_does_not_expand_to_top() {
        let heights = vec![50; 1000];
        let (start, end) = target_visible_range(&heights, 0.0, 500.0, 2, true);
        assert_eq!(end, 1000);
        assert!(start >= 980);
    }

    #[test]
    fn target_visible_range_when_unpinned_uses_scroll_offset() {
        let heights = vec![50; 1000];
        let (start, end) = target_visible_range(&heights, 0.0, 500.0, 2, false);
        assert_eq!(start, 0);
        assert!(end <= 30);
    }

    #[test]
    fn min_visible_end_fills_a_tall_viewport() {
        assert_eq!(min_visible_end(0, 80, 800.0), 42);
        assert_eq!(min_visible_end(10, 20, 800.0), 20);
        assert_eq!(min_visible_end(0, 3, 100.0), 3);
    }

    #[test]
    fn reused_heights_keep_unchanged_rows_and_prepend() {
        let old = vec!["a".into(), "b".into()];
        let heights = vec![10, 20];
        assert_eq!(
            reuse_row_heights(&old, &heights, &["a".into(), "b".into(), "c".into()]),
            vec![10, 20, ROW_ESTIMATE]
        );
        assert_eq!(
            reuse_row_heights(&old, &heights, &["z".into(), "a".into(), "b".into()]),
            vec![ROW_ESTIMATE, 10, 20]
        );
        assert_eq!(
            reuse_row_heights(&old, &heights, &["a".into(), "x".into()]),
            vec![10, ROW_ESTIMATE]
        );
    }

    #[test]
    fn layout_bounds_stack_from_the_top() {
        let rows = vec![
            r#"{"role":"YOU","body":"hi","images":[],"time":1,"kind":""}"#.into(),
            r#"{"role":"AGENT","body":"ok","images":[],"time":2,"kind":""}"#.into(),
        ];
        let bounds = row_layout_bounds(&rows, &[40, 60]);
        assert_eq!(bounds[0].top, 0.0);
        assert_eq!(bounds[0].bottom, 40.0);
        assert!(bounds[0].user);
        assert_eq!(bounds[1].top, 40.0);
        assert_eq!(bounds[1].bottom, 100.0);
        assert!(!bounds[1].user);
    }

    #[test]
    fn optimistic_user_row_stays_until_a_new_you_row_arrives() {
        let existing = vec![optimistic_transcript_row(
            &Draft {
                text: "old".into(),
                attachments: Vec::new(),
            },
            1,
        )];
        let pending = OptimisticPrompt {
            row: optimistic_transcript_row(
                &Draft {
                    text: "new".into(),
                    attachments: Vec::new(),
                },
                2,
            ),
            baseline_you: 1,
        };
        let (rows, superseded) = apply_optimistic_row(existing.clone(), Some(&pending));
        assert!(!superseded);
        assert_eq!(rows.len(), 2);
        let mut arrived = existing;
        arrived.push(pending.row.clone());
        let (rows, superseded) = apply_optimistic_row(arrived, Some(&pending));
        assert!(superseded);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter()
                .filter(|row| transcript_row_is_user(row))
                .count(),
            2
        );
    }

    #[test]
    fn agent_rows_do_not_replace_an_optimistic_user_row() {
        let pending = OptimisticPrompt {
            row: optimistic_transcript_row(
                &Draft {
                    text: "hello".into(),
                    attachments: Vec::new(),
                },
                2,
            ),
            baseline_you: 1,
        };
        let rows = vec![
            optimistic_transcript_row(
                &Draft {
                    text: "old".into(),
                    attachments: Vec::new(),
                },
                1,
            ),
            serde_json::json!({
                "role": "AGENT",
                "body": "working",
                "images": [],
                "time": 3,
                "kind": "",
            })
            .to_string(),
        ];
        let (rows, superseded) = apply_optimistic_row(rows, Some(&pending));
        assert!(!superseded);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn tabs_move_to_either_side_of_the_drop_target() {
        let mut tabs = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert!(move_tab(&mut tabs, "a", "b", true));
        assert_eq!(tabs, ["b", "a", "c"]);
        assert!(move_tab(&mut tabs, "c", "b", false));
        assert_eq!(tabs, ["c", "b", "a"]);
        assert!(!move_tab(&mut tabs, "c", "c", true));
        assert!(!move_tab(&mut tabs, "missing", "a", false));
    }

    #[test]
    fn settings_preserve_or_clear_the_existing_cloudflare_secret() {
        let config = config_with_cloudflare();

        assert_eq!(
            configured_cloudflare_credentials(
                &config,
                "https://opencode.example.com/",
                "client.access",
                "",
            )
            .unwrap(),
            config.cloudflare_access.clone()
        );
        assert_eq!(
            configured_cloudflare_credentials(&config, "https://opencode.example.com", "", "",)
                .unwrap(),
            None
        );
        assert!(configured_cloudflare_credentials(
            &config,
            "https://other.example.com",
            "client.access",
            "",
        )
        .is_err());
    }

    #[test]
    fn fuzzy_search_matches_subsequences_and_ranks_exact_matches_higher() {
        assert_eq!(fuzzy_score("", "claude-3-7-sonnet"), Some(0));
        assert!(fuzzy_score("xyz", "claude-3-7-sonnet").is_none());

        let c37 = fuzzy_score("c37", "anthropic / claude-3-7-sonnet");
        assert!(c37.is_some());

        let sonnet = fuzzy_score("sonnet", "anthropic / claude-3-7-sonnet").unwrap();
        let sparse = fuzzy_score("son", "anthropic / session-open-now").unwrap();
        assert!(sonnet > sparse);

        assert!(fuzzy_score("CLAUDE", "Claude 3.7 Sonnet").is_some());
        assert_eq!(
            fuzzy_score("claude", "Claude 3.7 Sonnet"),
            fuzzy_score("CLAUDE", "Claude 3.7 Sonnet")
        );
    }

    #[test]
    fn filter_tab_sessions_preserves_tab_order_when_empty_query() {
        let s1 = Session {
            id: "s1".into(),
            directory: "/a".into(),
            title: "First Tab".into(),
            time: SessionTime {
                created: 1,
                updated: 1,
                archived: None,
            },
            parent_id: None,
            agent: None,
            model: None,
        };
        let s2 = Session {
            id: "s2".into(),
            directory: "/b".into(),
            title: "Second Tab".into(),
            time: SessionTime {
                created: 2,
                updated: 2,
                archived: None,
            },
            parent_id: None,
            agent: None,
            model: None,
        };
        let sessions = vec![s1, s2];
        let filtered = filter_tab_sessions(&sessions, "");
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].id, "s1");
        assert_eq!(filtered[1].id, "s2");
    }

    #[test]
    fn filter_tab_sessions_filters_and_ranks_by_match() {
        let s1 = Session {
            id: "s1".into(),
            directory: "/work/docs".into(),
            title: "Documentation review".into(),
            time: SessionTime {
                created: 1,
                updated: 1,
                archived: None,
            },
            parent_id: None,
            agent: None,
            model: None,
        };
        let s2 = Session {
            id: "s2".into(),
            directory: "/work/core".into(),
            title: "Core engine refactor".into(),
            time: SessionTime {
                created: 2,
                updated: 2,
                archived: None,
            },
            parent_id: None,
            agent: None,
            model: None,
        };
        let sessions = vec![s1, s2];
        let filtered = filter_tab_sessions(&sessions, "refactor");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "s2");
    }

    #[test]
    fn filter_all_sessions_sorts_newest_first_when_query_empty() {
        let s1 = Session {
            id: "s1".into(),
            directory: "/a".into(),
            title: "First Tab".into(),
            time: SessionTime {
                created: 10,
                updated: 10,
                archived: None,
            },
            parent_id: None,
            agent: None,
            model: None,
        };
        let s2 = Session {
            id: "s2".into(),
            directory: "/b".into(),
            title: "Second Tab".into(),
            time: SessionTime {
                created: 20,
                updated: 20,
                archived: None,
            },
            parent_id: None,
            agent: None,
            model: None,
        };
        let sessions = vec![s1, s2];
        let filtered = filter_all_sessions(&sessions, "");
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].id, "s2");
        assert_eq!(filtered[1].id, "s1");
    }

    #[test]
    fn filter_all_sessions_ranks_by_score_when_query_provided() {
        let s1 = Session {
            id: "s1".into(),
            directory: "/work/docs".into(),
            title: "Documentation review".into(),
            time: SessionTime {
                created: 10,
                updated: 10,
                archived: None,
            },
            parent_id: None,
            agent: None,
            model: None,
        };
        let s2 = Session {
            id: "s2".into(),
            directory: "/work/core".into(),
            title: "Core engine refactor".into(),
            time: SessionTime {
                created: 20,
                updated: 20,
                archived: None,
            },
            parent_id: None,
            agent: None,
            model: None,
        };
        let sessions = vec![s1, s2];
        let filtered = filter_all_sessions(&sessions, "doc");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "s1");
    }
}
