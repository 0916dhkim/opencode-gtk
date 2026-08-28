use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::{Rc, Weak},
    time::Duration,
};

use async_channel::Receiver;
use gtk::{gdk, gio, glib, pango, prelude::*};

use crate::{
    api::{ApiConfig, ApiHandle, Bootstrap, Command, MessagePage, ServerEnvelope, UiEvent},
    credentials::{self, CloudflareAccessCredentials},
    markdown,
    model::{
        deleted_session_id, event_data, event_run_status, event_session, event_session_id,
        Conversation, ModelCatalog, ModelOption, ModelSelection, Project, RunStatus, Session,
        SessionTime,
    },
    persist::{
        default_path, ConnectionSettings, PersistedState, PersistedTab, ServerState,
        ThemePreference,
    },
};

const STREAM_FRAME: Duration = Duration::from_millis(33);
const BOOTSTRAP_RETRY_MIN: Duration = Duration::from_secs(2);
const BOOTSTRAP_RETRY_MAX: Duration = Duration::from_secs(30);
const SESSION_PICKER_LIMIT: usize = 200;
const BOTTOM_EPSILON: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TranscriptUpdate {
    Content,
    Prepend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TranscriptIndicator {
    Hidden,
    NoSession,
    Loading,
    Refreshing,
    Working,
    Error,
    Empty,
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

#[derive(Clone)]
struct QuestionInputs {
    options: Vec<(gtk::CheckButton, String)>,
    custom: Option<gtk::Entry>,
    multiple: bool,
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
    drafts: HashMap<String, Draft>,
    pending_prompts: HashMap<String, PendingPrompt>,
    server_busy: HashSet<String>,
    abort_requested: HashSet<String>,
    loading_messages: HashSet<String>,
    loading_models: HashSet<String>,
}

#[derive(Clone)]
struct Widgets {
    window: gtk::ApplicationWindow,
    session_button: gtk::Button,
    new_button: gtk::Button,
    settings_button: gtk::Button,
    status: gtk::Label,
    tab_bar: gtk::Box,
    transcript_model: gtk::StringList,
    transcript_scroll: gtk::ScrolledWindow,
    transcript_status: gtk::Box,
    transcript_spinner: gtk::Spinner,
    transcript_status_label: gtk::Label,
    load_earlier: gtk::Button,
    composer: gtk::TextView,
    attachment_box: gtk::Box,
    attach_button: gtk::Button,
    model_store: gtk::StringList,
    model_dropdown: gtk::DropDown,
    variant_store: gtk::StringList,
    variant_dropdown: gtk::DropDown,
    send_button: gtk::Button,
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
    state: State,
    widgets: Widgets,
    css_provider: gtk::CssProvider,
    theme: ThemePreference,
    rendered_session: Option<String>,
    rendered_rows: Vec<String>,
    transcript_at_bottom: bool,
    transcript_scroll_generation: u64,
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
    dialogs: HashMap<String, gtk::Window>,
    pending_actions: HashSet<String>,
    new_session_dialog: Option<gtk::Window>,
    settings_dialog: Option<gtk::Window>,
    next_session_request_id: u64,
    pending_session_request: Option<u64>,
    next_prompt_request_id: u64,
    connected_once: bool,
    event_connected: bool,
}

pub fn launch(
    application: &gtk::Application,
    server: Option<String>,
    username: Option<String>,
    password: Option<String>,
    cf_access_client_id: Option<String>,
    cf_access_client_secret: Option<String>,
) {
    let state_path = default_path();
    let (persisted, persistence_warning, persistence_error) =
        match PersistedState::load(&state_path) {
            Ok((persisted, warning)) => (persisted, warning, None),
            Err(error) => (PersistedState::default(), None, Some(error.to_string())),
        };
    let base_url = server.unwrap_or_else(|| persisted.connection.server.clone());
    let load_stored_cloudflare = persisted.connection.cloudflare_access
        && base_url.trim_end_matches('/') == persisted.connection.server.trim_end_matches('/');
    let (cloudflare_access, credential_warning) = initial_cloudflare_credentials(
        &base_url,
        load_stored_cloudflare,
        cf_access_client_id,
        cf_access_client_secret,
    );
    let config = ApiConfig {
        base_url,
        username: username.unwrap_or_else(|| persisted.connection.username.clone()),
        password,
        cloudflare_access,
    };
    let theme = persisted.theme;
    let css_provider = install_css(theme);
    let widgets = build_widgets(application);
    let (api, events, server_key) = match ApiHandle::start(config.clone()) {
        Ok(started) => started,
        Err(error) => {
            widgets.status.set_label(&error.to_string());
            widgets.status.add_css_class("error");
            widgets.window.present();
            return;
        }
    };

    let persistence_writes_blocked = persistence_error.is_some();
    let had_server_state = persisted.servers.contains_key(&server_key);
    let server_state = persisted
        .servers
        .get(&server_key)
        .cloned()
        .unwrap_or_default();
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
        state,
        widgets,
        css_provider,
        theme,
        rendered_session: None,
        rendered_rows: Vec::new(),
        transcript_at_bottom: true,
        transcript_scroll_generation: 0,
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
        new_session_dialog: None,
        settings_dialog: None,
        next_session_request_id: 0,
        pending_session_request: None,
        next_prompt_request_id: 0,
        connected_once: false,
        event_connected: false,
    }));
    controller.borrow_mut().self_weak = Rc::downgrade(&controller);
    {
        let this = controller.borrow();
        this.widgets.status.set_tooltip_text(Some(&this.server_key));
    }

    wire_callbacks(&controller);
    Controller::refresh_all(&controller);
    controller.borrow().widgets.window.present();
    controller.borrow().api.send(Command::Bootstrap);

    let close_controller = Rc::downgrade(&controller);
    controller
        .borrow()
        .widgets
        .window
        .connect_close_request(move |_| {
            if let Some(controller) = close_controller.upgrade() {
                controller.borrow().events.close();
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
        .build();

    let header = gtk::HeaderBar::new();
    let session_button = gtk::Button::with_label("Sessions");
    session_button.add_css_class("flat");
    let new_button = gtk::Button::with_label("New session");
    new_button.set_tooltip_text(Some("New session (Ctrl+T)"));
    new_button.add_css_class("flat");
    let settings_button = gtk::Button::with_label("Settings");
    settings_button.set_tooltip_text(Some("Server connection and appearance (Ctrl+,)"));
    settings_button.add_css_class("flat");
    let title = gtk::Label::new(Some("OpenCode"));
    title.add_css_class("app-title");
    let status = gtk::Label::new(Some("Connecting"));
    status.add_css_class("connection-status");
    let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    title_box.append(&title);
    title_box.append(&status);
    header.set_title_widget(Some(&title_box));
    header.pack_start(&session_button);
    header.pack_end(&settings_button);
    header.pack_end(&new_button);
    window.set_titlebar(Some(&header));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let tab_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tab_bar.set_margin_start(10);
    tab_bar.set_margin_end(10);
    tab_bar.set_margin_top(7);
    tab_bar.set_margin_bottom(7);
    let tab_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&tab_bar)
        .build();
    tab_scroll.add_css_class("tab-strip");
    root.append(&tab_scroll);

    let load_earlier = gtk::Button::with_label("Load earlier messages");
    load_earlier.add_css_class("flat");
    load_earlier.set_visible(false);

    let transcript_model = gtk::StringList::new(&[]);
    let selection = gtk::NoSelection::new(Some(transcript_model.clone()));
    let factory = transcript_factory();
    let transcript = gtk::ListView::new(Some(selection), Some(factory));
    transcript.add_css_class("transcript");
    let transcript_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&transcript)
        .build();
    let transcript_spinner = gtk::Spinner::new();
    let transcript_status_label = gtk::Label::new(Some("Open a session to begin"));
    let transcript_status = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    transcript_status.add_css_class("transcript-status");
    transcript_status.set_halign(gtk::Align::Center);
    transcript_status.set_valign(gtk::Align::Center);
    transcript_status.set_can_target(false);
    transcript_status.append(&transcript_spinner);
    transcript_status.append(&transcript_status_label);
    let overlay = gtk::Overlay::new();
    overlay.set_vexpand(true);
    overlay.set_child(Some(&transcript_scroll));
    overlay.add_overlay(&transcript_status);
    let conversation = gtk::Box::new(gtk::Orientation::Vertical, 0);
    conversation.set_vexpand(true);
    conversation.append(&load_earlier);
    conversation.append(&overlay);
    root.append(&conversation);

    let composer = gtk::TextView::new();
    composer.set_wrap_mode(gtk::WrapMode::WordChar);
    composer.set_accepts_tab(false);
    composer.set_top_margin(10);
    composer.set_bottom_margin(10);
    composer.set_left_margin(12);
    composer.set_right_margin(12);
    composer.set_height_request(94);
    composer.add_css_class("composer-input");
    let composer_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .max_content_height(220)
        .child(&composer)
        .build();

    let attachment_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    attachment_box.set_visible(false);
    let attach_button = gtk::Button::with_label("Attach");
    attach_button.set_tooltip_text(Some("Attach files (Ctrl+U)"));
    let model_store = gtk::StringList::new(&["Loading models..."]);
    let model_dropdown = gtk::DropDown::new(Some(model_store.clone()), None::<gtk::Expression>);
    model_dropdown.set_hexpand(true);
    model_dropdown.set_sensitive(false);
    let variant_store = gtk::StringList::new(&["Default"]);
    let variant_dropdown = gtk::DropDown::new(Some(variant_store.clone()), None::<gtk::Expression>);
    variant_dropdown.set_sensitive(false);
    variant_dropdown.set_tooltip_text(Some("Reasoning level"));
    let send_button = gtk::Button::with_label("Send");
    send_button.add_css_class("suggested-action");
    send_button.set_sensitive(false);

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.append(&attach_button);
    controls.append(&model_dropdown);
    controls.append(&variant_dropdown);
    controls.append(&send_button);

    let composer_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    composer_box.set_margin_start(14);
    composer_box.set_margin_end(14);
    composer_box.set_margin_top(10);
    composer_box.set_margin_bottom(12);
    composer_box.append(&attachment_box);
    composer_box.append(&composer_scroll);
    composer_box.append(&controls);
    let composer_frame = gtk::Frame::new(None);
    composer_frame.add_css_class("composer-frame");
    composer_frame.set_margin_start(18);
    composer_frame.set_margin_end(18);
    composer_frame.set_margin_bottom(16);
    composer_frame.set_child(Some(&composer_box));
    root.append(&composer_frame);

    window.set_child(Some(&root));

    Widgets {
        window,
        session_button,
        new_button,
        settings_button,
        status,
        tab_bar,
        transcript_model,
        transcript_scroll,
        transcript_status,
        transcript_spinner,
        transcript_status_label,
        load_earlier,
        composer,
        attachment_box,
        attach_button,
        model_store,
        model_dropdown,
        variant_store,
        variant_dropdown,
        send_button,
    }
}

fn transcript_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let row = gtk::Box::new(gtk::Orientation::Vertical, 6);
        row.add_css_class("message-row");
        let role = gtk::Label::new(None);
        role.set_xalign(0.0);
        role.add_css_class("message-role");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
        content.set_hexpand(true);
        content.add_css_class("message-content");
        row.append(&role);
        row.append(&content);
        item.set_child(Some(&row));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(object) = item.item().and_downcast::<gtk::StringObject>() else {
            return;
        };
        let Some(row) = item.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let Some(role) = row.first_child().and_downcast::<gtk::Label>() else {
            return;
        };
        let Some(content) = row.last_child().and_downcast::<gtk::Box>() else {
            return;
        };
        let value = object.string();
        let (role_text, body) = value
            .split_once('\n')
            .unwrap_or(("OPENCODE", value.as_str()));
        role.set_label(role_text);
        if role_text == "YOU" {
            clear_box(&content);
            let label = gtk::Label::new(Some(body));
            label.set_xalign(0.0);
            label.set_yalign(0.0);
            label.set_wrap(true);
            label.set_wrap_mode(pango::WrapMode::WordChar);
            label.set_selectable(true);
            label.add_css_class("message-plain-text");
            content.append(&label);
        } else {
            markdown::render_into(&content, body);
        }
        row.remove_css_class("user-message");
        row.remove_css_class("assistant-message");
        row.add_css_class(if role_text == "YOU" {
            "user-message"
        } else {
            "assistant-message"
        });
    });
    factory
}

fn wire_callbacks(controller: &Rc<RefCell<Controller>>) {
    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .transcript_scroll
        .vadjustment()
        .connect_value_changed(move |adjustment| {
            let Some(controller) = weak.upgrade() else {
                return;
            };
            let Ok(mut controller) = controller.try_borrow_mut() else {
                return;
            };
            controller.transcript_at_bottom = adjustment_at_bottom(adjustment);
        });

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
    controller
        .borrow()
        .widgets
        .settings_button
        .connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::show_settings(&controller);
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
    let weak = Rc::downgrade(controller);
    key.connect_key_pressed(move |_, key, _, modifiers| {
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
        .model_dropdown
        .connect_selected_notify(move |dropdown| {
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
            let Some(model) = controller
                .current_models
                .get(dropdown.selected() as usize)
                .cloned()
            else {
                return;
            };
            controller.state.selections.insert(
                active,
                ModelSelection {
                    provider_id: model.provider_id,
                    model_id: model.model_id,
                    variant: None,
                },
            );
            controller.refresh_variant_control();
            controller.refresh_attachment_control();
            controller.persist_state();
            controller.refresh_send_button();
        });

    let weak = Rc::downgrade(controller);
    controller
        .borrow()
        .widgets
        .variant_dropdown
        .connect_selected_notify(move |dropdown| {
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
            let variant = controller
                .current_variants
                .get(dropdown.selected() as usize)
                .cloned()
                .flatten();
            if let Some(selection) = controller.state.selections.get_mut(&active) {
                selection.variant = variant;
                controller.persist_state();
            }
        });

    let shortcuts = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(controller);
    shortcuts.connect_key_pressed(move |_, key, _, modifiers| {
        if !modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
            return glib::Propagation::Proceed;
        }
        let Some(controller) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        match key {
            gdk::Key::comma => Controller::show_settings(&controller),
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
            gdk::Key::_1 => Controller::select_tab_number(&controller, 0),
            gdk::Key::_2 => Controller::select_tab_number(&controller, 1),
            gdk::Key::_3 => Controller::select_tab_number(&controller, 2),
            gdk::Key::_4 => Controller::select_tab_number(&controller, 3),
            gdk::Key::_5 => Controller::select_tab_number(&controller, 4),
            gdk::Key::_6 => Controller::select_tab_number(&controller, 5),
            gdk::Key::_7 => Controller::select_tab_number(&controller, 6),
            gdk::Key::_8 => Controller::select_tab_number(&controller, 7),
            gdk::Key::_9 => Controller::select_tab_number(&controller, 8),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    controller.borrow().widgets.window.add_controller(shortcuts);
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
                    let dialog = {
                        let mut this = controller.borrow_mut();
                        this.upsert_session(session.clone());
                        if this.pending_session_request == Some(request_id) {
                            this.pending_session_request = None;
                            this.new_session_dialog.take()
                        } else {
                            None
                        }
                    };
                    if let Some(dialog) = dialog {
                        dialog.close();
                    }
                    Self::open_tab(controller, &session.id);
                }
                Err(error) => {
                    let mut this = controller.borrow_mut();
                    if this.pending_session_request == Some(request_id) {
                        this.pending_session_request = None;
                        if let Some(dialog) = &this.new_session_dialog {
                            dialog.set_sensitive(true);
                        }
                    }
                    this.show_error(&error);
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
                            this.state.pending_prompts.remove(&session_id);
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
                let dialog = {
                    let mut this = controller.borrow_mut();
                    this.pending_actions.remove(&request_id);
                    match result {
                        Ok(()) => this.dialogs.remove(&request_id),
                        Err(error) => {
                            if let Some(dialog) = this.dialogs.get(&request_id) {
                                dialog.set_sensitive(true);
                            }
                            this.show_error(&error);
                            None
                        }
                    }
                };
                if let Some(dialog) = dialog {
                    dialog.close();
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
        let mut stale_dialogs = Vec::new();
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
                    RunStatus::Busy => {
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
                        this.state.pending_prompts.remove(&session_id);
                        this.state.abort_requested.remove(&session_id);
                    }
                }
            }
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
                    if let Some(dialog) = this.dialogs.remove(&id) {
                        this.pending_actions.remove(&id);
                        stale_dialogs.push(dialog);
                    }
                }
            }
            let known: HashSet<_> = this
                .state
                .sessions
                .iter()
                .map(|session| session.id.clone())
                .collect();
            if bootstrap.sessions_complete {
                this.state.tabs.retain(|id| known.contains(id));
                this.state.conversations.retain(|id, _| known.contains(id));
                this.state.selections.retain(|id, _| known.contains(id));
                this.state.drafts.retain(|id, _| known.contains(id));
                this.state
                    .pending_prompts
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
        for dialog in stale_dialogs {
            dialog.close();
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
        let mut dialogs_to_close = Vec::new();
        let mut api_commands = Vec::new();
        let mut this = controller.borrow_mut();
        this.event_flush_scheduled = false;
        let events = std::mem::take(&mut this.pending_events);
        let active = this.state.active.clone();
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
                        .insert(session_id.clone(), status);
                }
                match status {
                    RunStatus::Busy => {
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
                        this.state.pending_prompts.remove(&session_id);
                    }
                }
                let status_changed = this.update_session_status(&session_id, status);
                tab_status_changed |=
                    status_changed && this.state.tabs.iter().any(|id| id == &session_id);
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
                        if let Some(dialog) = this.dialogs.remove(request_id) {
                            this.pending_actions.remove(request_id);
                            dialogs_to_close.push(dialog);
                        }
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
            let weak = this.self_weak.clone();
            this.refresh_tabs(&weak);
        }
        if transcript_changed {
            this.refresh_transcript(TranscriptUpdate::Content);
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
        for dialog in dialogs_to_close {
            dialog.close();
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
        clear_box(&self.widgets.tab_bar);
        for id in self.state.tabs.clone() {
            let title = self
                .session(&id)
                .map(|session| session.title.clone())
                .unwrap_or_else(|| "Unknown session".to_owned());
            let tab = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            tab.add_css_class("session-tab");
            if self.state.active.as_deref() == Some(id.as_str()) {
                tab.add_css_class("active");
            }
            let busy = self.state.statuses.get(&id) == Some(&RunStatus::Busy);
            let status = if busy {
                let spinner = gtk::Spinner::new();
                spinner.start();
                spinner.upcast::<gtk::Widget>()
            } else {
                gtk::Box::new(gtk::Orientation::Horizontal, 0).upcast::<gtk::Widget>()
            };
            status.add_css_class("session-tab-status");
            status.add_css_class(if busy { "busy" } else { "idle" });
            status.set_halign(gtk::Align::Center);
            status.set_valign(gtk::Align::Center);
            status.set_tooltip_text(Some(if busy {
                "Session is working"
            } else {
                "Session is idle"
            }));
            let select = gtk::Button::with_label(&title);
            select.set_tooltip_text(self.session(&id).map(|session| session.directory.as_str()));
            select.add_css_class("flat");
            let close = gtk::Button::with_label("×");
            close.set_tooltip_text(Some("Close tab"));
            close.add_css_class("flat");
            tab.append(&status);
            tab.append(&select);
            tab.append(&close);

            let weak_select = weak.clone();
            let select_id = id.clone();
            select.connect_clicked(move |_| {
                if let Some(controller) = weak_select.upgrade() {
                    Self::activate_tab(&controller, &select_id);
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
            middle.connect_released(move |_, _, _, _| {
                if let Some(controller) = weak_middle.upgrade() {
                    Self::close_tab(&controller, &id);
                }
            });
            select.add_controller(middle);
            self.widgets.tab_bar.append(&tab);
        }
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
            if active_changed {
                this.transcript_at_bottom = true;
                this.transcript_scroll_generation += 1;
                this.persist_state();
            }
            (
                load_messages.then_some(session.clone()),
                load_models.then_some(session),
            )
        };
        Self::refresh_all(controller);
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
        this.widgets.composer.grab_focus();
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

    fn refresh_composer(&mut self) {
        self.controls_updating = true;
        let active = self.state.active.clone();
        let draft = active
            .as_ref()
            .and_then(|active| self.state.drafts.get(active))
            .cloned()
            .unwrap_or_default();
        self.widgets.composer.buffer().set_text(&draft.text);
        self.widgets.composer.set_editable(active.is_some());
        self.refresh_attachments();
        self.refresh_model_control();
        self.controls_updating = false;
        self.refresh_send_button();
    }

    fn refresh_model_control(&mut self) {
        self.controls_updating = true;
        self.widgets.model_dropdown.set_tooltip_text(None);
        self.widgets.attach_button.set_sensitive(false);
        self.widgets
            .attach_button
            .set_tooltip_text(Some("Select a model that accepts attachments"));
        let Some(active) = self.state.active.clone() else {
            self.current_models.clear();
            replace_string_list(&self.widgets.model_store, &["No session"]);
            self.widgets.model_dropdown.set_sensitive(false);
            self.refresh_variant_control();
            self.controls_updating = false;
            return;
        };
        let Some(directory) = self
            .session(&active)
            .map(|session| session.directory.clone())
        else {
            self.controls_updating = false;
            return;
        };
        let Some(catalog) = self.state.catalogs.get(&directory).cloned() else {
            self.current_models.clear();
            let loading = self.state.loading_models.contains(&directory);
            let error = self.model_load_errors.get(&directory);
            replace_string_list(
                &self.widgets.model_store,
                &[if loading {
                    "Loading models..."
                } else if error.is_some() {
                    "Could not load models"
                } else {
                    "No models available"
                }],
            );
            self.widgets
                .model_dropdown
                .set_tooltip_text(error.map(String::as_str));
            self.widgets.model_dropdown.set_selected(0);
            self.widgets.model_dropdown.set_sensitive(false);
            self.refresh_variant_control();
            self.controls_updating = false;
            return;
        };

        self.widgets.model_dropdown.set_tooltip_text(None);
        self.current_models = catalog.models.clone();
        if self.current_models.is_empty() {
            self.state.selections.remove(&active);
            replace_string_list(&self.widgets.model_store, &["No models available"]);
            self.widgets.model_dropdown.set_selected(0);
            self.widgets.model_dropdown.set_sensitive(false);
            self.refresh_variant_control();
            self.controls_updating = false;
            return;
        }
        let labels: Vec<_> = self
            .current_models
            .iter()
            .map(|model| model.label.as_str())
            .collect();
        replace_string_list(&self.widgets.model_store, &labels);
        let session_selection = self
            .session(&active)
            .and_then(Session::model_selection)
            .filter(|selection| catalog.find(selection).is_some());
        let selection = self
            .state
            .selections
            .get(&active)
            .filter(|selection| catalog.find(selection).is_some())
            .cloned()
            .or(session_selection)
            .or(catalog.preferred)
            .or_else(|| {
                self.current_models.first().map(|model| ModelSelection {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                    variant: None,
                })
            });
        if let Some(selection) = selection {
            let index = self
                .current_models
                .iter()
                .position(|model| {
                    model.provider_id == selection.provider_id
                        && model.model_id == selection.model_id
                })
                .unwrap_or_default();
            self.state.selections.insert(active, selection);
            self.widgets.model_dropdown.set_selected(index as u32);
        }
        let model_ready = !self.state.loading_models.contains(&directory)
            && !self.model_load_errors.contains_key(&directory);
        if self.state.loading_models.contains(&directory) {
            self.widgets
                .model_dropdown
                .set_tooltip_text(Some("Refreshing models..."));
        } else if let Some(error) = self.model_load_errors.get(&directory) {
            self.widgets.model_dropdown.set_tooltip_text(Some(error));
        }
        self.widgets.model_dropdown.set_sensitive(model_ready);
        self.refresh_variant_control();
        self.refresh_attachment_control();
        self.controls_updating = false;
    }

    fn refresh_variant_control(&mut self) {
        let selection = self
            .state
            .active
            .as_ref()
            .and_then(|active| self.state.selections.get(active))
            .cloned();
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
        let labels: Vec<_> = self
            .current_variants
            .iter()
            .map(|variant| variant.as_deref().unwrap_or("Default"))
            .collect();
        replace_string_list(&self.widgets.variant_store, &labels);
        let selected = selection
            .as_ref()
            .and_then(|selection| {
                self.current_variants
                    .iter()
                    .position(|variant| variant == &selection.variant)
            })
            .unwrap_or_else(|| {
                if selection
                    .as_ref()
                    .is_some_and(|selection| selection.variant.is_some())
                {
                    if let Some(active) = self.state.active.as_ref() {
                        if let Some(selection) = self.state.selections.get_mut(active) {
                            selection.variant = None;
                        }
                    }
                    self.persist_state();
                }
                0
            });
        self.widgets.variant_dropdown.set_selected(selected as u32);
        self.widgets
            .variant_dropdown
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
            self.widgets.send_button.set_label("Send");
            self.widgets.send_button.set_sensitive(false);
            return;
        };
        let busy = self.state.statuses.get(active) == Some(&RunStatus::Busy);
        self.widgets
            .send_button
            .set_label(if busy { "Stop" } else { "Send" });
        let draft = self.state.drafts.get(active);
        let has_input = draft
            .is_some_and(|draft| !draft.text.trim().is_empty() || !draft.attachments.is_empty());
        let attachments_valid = draft.is_none_or(|draft| {
            draft.attachments.is_empty() || self.selected_model_supports_attachments()
        });
        let model_ready = self.session(active).is_some_and(|session| {
            !self.state.loading_models.contains(&session.directory)
                && !self.model_load_errors.contains_key(&session.directory)
                && self.state.selections.get(active).is_some_and(|selection| {
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
        let Some(active) = self.state.active.as_ref() else {
            return false;
        };
        let Some(selection) = self.state.selections.get(active) else {
            return false;
        };
        self.current_models.iter().any(|model| {
            model.provider_id == selection.provider_id
                && model.model_id == selection.model_id
                && model.supports_attachments
        })
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

    fn refresh_transcript(&mut self, update: TranscriptUpdate) {
        let active = self.state.active.clone();
        let rows = active
            .as_ref()
            .and_then(|active| self.state.conversations.get(active))
            .map(Conversation::rendered_rows)
            .unwrap_or_default();
        let adjustment = self.widgets.transcript_scroll.vadjustment();
        let old_upper = adjustment.upper();
        let old_value = adjustment.value();
        if transcript_session_changed(self.rendered_session.as_deref(), active.as_deref()) {
            self.transcript_at_bottom = true;
            self.transcript_scroll_generation += 1;
        }
        let follow_bottom = self.transcript_at_bottom;
        sync_transcript_list(
            &self.widgets.transcript_model,
            &mut self.rendered_session,
            &mut self.rendered_rows,
            active.as_deref(),
            rows,
        );
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
        let working = active
            .as_ref()
            .is_some_and(|active| self.state.statuses.get(active) == Some(&RunStatus::Busy));
        let load_error = active
            .as_ref()
            .and_then(|active| self.message_load_errors.get(active));
        let has_rows = !self.rendered_rows.is_empty();
        let indicator = transcript_indicator(
            active.is_some(),
            loading,
            replacing,
            loaded,
            has_rows,
            working,
            load_error.is_some(),
        );
        self.refresh_transcript_indicator(indicator, has_rows, load_error.map(String::as_str));
        let next_cursor = active
            .as_ref()
            .and_then(|active| self.state.conversations.get(active))
            .and_then(|conversation| conversation.next_cursor.as_ref());
        self.widgets.load_earlier.set_visible(next_cursor.is_some());
        self.widgets.load_earlier.set_sensitive(!loading);
        self.widgets
            .load_earlier
            .set_label(if loading && !replacing {
                "Loading earlier messages..."
            } else {
                "Load earlier messages"
            });

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
        } else if follow_bottom {
            glib::idle_add_local_once(move || {
                let should_follow = weak.upgrade().is_some_and(|controller| {
                    let controller = controller.borrow();
                    controller.transcript_scroll_generation == generation
                        && controller.state.active == active
                        && controller.transcript_at_bottom
                });
                if should_follow {
                    let bottom = adjustment.upper() - adjustment.page_size();
                    adjustment.set_value(clamp_adjustment(&adjustment, bottom));
                }
            });
        }
    }

    fn refresh_transcript_indicator(
        &self,
        indicator: TranscriptIndicator,
        has_rows: bool,
        error: Option<&str>,
    ) {
        let (label, spinning, alignment) = match indicator {
            TranscriptIndicator::Hidden => ("", false, gtk::Align::Center),
            TranscriptIndicator::NoSession => {
                ("Open a session to begin", false, gtk::Align::Center)
            }
            TranscriptIndicator::Loading => ("Loading conversation", true, gtk::Align::Center),
            TranscriptIndicator::Refreshing => ("Refreshing conversation", true, gtk::Align::Start),
            TranscriptIndicator::Working => ("OpenCode is working", true, gtk::Align::End),
            TranscriptIndicator::Error => (
                if has_rows {
                    "Could not refresh conversation"
                } else {
                    "Could not load conversation"
                },
                false,
                if has_rows {
                    gtk::Align::Start
                } else {
                    gtk::Align::Center
                },
            ),
            TranscriptIndicator::Empty => ("No messages yet", false, gtk::Align::Center),
        };
        let visible = indicator != TranscriptIndicator::Hidden;
        let compact = visible && has_rows;
        self.widgets.transcript_status.set_visible(visible);
        self.widgets.transcript_status.set_valign(alignment);
        self.widgets
            .transcript_status
            .set_margin_top(if compact { 12 } else { 0 });
        self.widgets
            .transcript_status
            .set_margin_bottom(if compact { 12 } else { 0 });
        if compact {
            self.widgets
                .transcript_status
                .add_css_class("transcript-status-compact");
        } else {
            self.widgets
                .transcript_status
                .remove_css_class("transcript-status-compact");
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
        let previous = self.state.statuses.insert(session_id.to_owned(), status);
        if status_transitioned_to_idle(previous, status) {
            self.notify_session_idle(session_id);
        }
        previous != Some(status)
    }

    fn notify_session_idle(&self, session_id: &str) {
        let Some(application) = self.widgets.window.application() else {
            return;
        };
        let title = self
            .session(session_id)
            .map(|session| format!("{} is idle", session.title))
            .unwrap_or_else(|| "OpenCode session is idle".to_owned());
        let notification = gio::Notification::new(&title);
        notification.set_body(Some("Ready for your next prompt."));
        application.send_notification(None, &notification);
    }

    fn send_if_idle(controller: &Rc<RefCell<Self>>) {
        let running = {
            let this = controller.borrow();
            this.state
                .active
                .as_ref()
                .and_then(|active| this.state.statuses.get(active))
                == Some(&RunStatus::Busy)
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
            if this.state.statuses.get(&active) == Some(&RunStatus::Busy) {
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
                let Some(selection) = this.state.selections.get(&active).cloned() else {
                    this.show_error("Select a model before sending");
                    return;
                };
                if this.state.loading_models.contains(&session.directory)
                    || this.model_load_errors.contains_key(&session.directory)
                    || this
                        .state
                        .catalogs
                        .get(&session.directory)
                        .is_none_or(|catalog| catalog.find(&selection).is_none())
                {
                    this.show_error("Wait for the model list to refresh before sending");
                    return;
                }
                let supports_attachments = this.selected_model_supports_attachments();
                let draft = this.state.drafts.entry(active.clone()).or_default();
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
                this.state.pending_prompts.insert(
                    active.clone(),
                    PendingPrompt {
                        request_id,
                        draft: pending,
                    },
                );
                let status_changed = this.update_session_status(&active, RunStatus::Busy);
                if status_changed {
                    let weak = this.self_weak.clone();
                    this.refresh_tabs(&weak);
                }
                this.refresh_composer();
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

    fn show_session_picker(controller: &Rc<RefCell<Self>>) {
        let this = controller.borrow();
        let dialog = gtk::Window::builder()
            .title("Open session")
            .transient_for(&this.widgets.window)
            .modal(true)
            .default_width(620)
            .default_height(620)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.set_margin_top(14);
        root.set_margin_bottom(14);
        root.set_margin_start(14);
        root.set_margin_end(14);
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Search sessions or directories"));
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .build();
        root.append(&search);
        root.append(&scroll);
        dialog.set_child(Some(&root));
        drop(this);

        {
            let this = controller.borrow();
            populate_session_list(
                &list,
                &this.state.sessions,
                "",
                Rc::downgrade(controller),
                dialog.clone(),
            );
        }
        search.connect_search_changed({
            let list = list.clone();
            let weak = Rc::downgrade(controller);
            let dialog = dialog.clone();
            move |search| {
                if let Some(controller) = weak.upgrade() {
                    let this = controller.borrow();
                    populate_session_list(
                        &list,
                        &this.state.sessions,
                        search.text().as_str(),
                        Rc::downgrade(&controller),
                        dialog.clone(),
                    );
                }
            }
        });
        dialog.present();
        search.grab_focus();
    }

    fn show_settings(controller: &Rc<RefCell<Self>>) {
        if let Some(dialog) = controller.borrow().settings_dialog.clone() {
            dialog.present();
            return;
        }
        let (parent, config, theme) = {
            let this = controller.borrow();
            (
                this.widgets.window.clone(),
                this.connection_config.clone(),
                this.theme,
            )
        };
        let dialog = gtk::Window::builder()
            .title("Settings")
            .transient_for(&parent)
            .modal(true)
            .default_width(560)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.set_margin_top(18);
        root.set_margin_bottom(18);
        root.set_margin_start(18);
        root.set_margin_end(18);

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

        let theme_label = gtk::Label::new(Some("Appearance"));
        theme_label.set_xalign(0.0);
        let theme_control = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let dark_theme = gtk::CheckButton::with_mnemonic("_Dark");
        let light_theme = gtk::CheckButton::with_mnemonic("_Light");
        light_theme.set_group(Some(&dark_theme));
        match theme {
            ThemePreference::Dark => dark_theme.set_active(true),
            ThemePreference::Light => light_theme.set_active(true),
        }
        theme_control.append(&dark_theme);
        theme_control.append(&light_theme);

        let validation = gtk::Label::new(None);
        validation.set_xalign(0.0);
        validation.set_wrap(true);
        validation.add_css_class("error");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_mnemonic("_Cancel");
        let apply = gtk::Button::with_mnemonic("_Apply");
        apply.add_css_class("suggested-action");
        actions.append(&cancel);
        actions.append(&apply);

        root.append(&server_label);
        root.append(&server);
        root.append(&username_label);
        root.append(&username);
        root.append(&password_label);
        root.append(&password);
        root.append(&connection_hint);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&cloudflare_label);
        root.append(&cloudflare_client_id_label);
        root.append(&cloudflare_client_id);
        root.append(&cloudflare_client_secret_label);
        root.append(&cloudflare_client_secret);
        root.append(&cloudflare_hint);
        root.append(&theme_label);
        root.append(&theme_control);
        root.append(&validation);
        root.append(&actions);
        dialog.set_child(Some(&root));
        dialog.set_default_widget(Some(&apply));
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

        cancel.connect_clicked({
            let dialog = dialog.clone();
            move |_| dialog.close()
        });
        apply.connect_clicked({
            let weak = Rc::downgrade(controller);
            let dialog = dialog.clone();
            let server = server.clone();
            let username = username.clone();
            let password = password.clone();
            let cloudflare_client_id = cloudflare_client_id.clone();
            let cloudflare_client_secret = cloudflare_client_secret.clone();
            let light_theme = light_theme.clone();
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
                let theme = if light_theme.is_active() {
                    ThemePreference::Light
                } else {
                    ThemePreference::Dark
                };
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
                    this.apply_preferences(&config, theme);
                    drop(this);
                    dialog.close();
                    return;
                }

                match ApiHandle::start(config.clone()) {
                    Ok((api, events, server_key)) => {
                        if let Err(error) = persist_cloudflare_credentials(&current, &config) {
                            events.close();
                            validation.set_label(&error.to_string());
                            return;
                        }
                        Self::switch_connection(
                            &controller,
                            config,
                            theme,
                            api,
                            events,
                            server_key,
                        );
                        dialog.close();
                    }
                    Err(error) => validation.set_label(&error.to_string()),
                }
            }
        });
        let settings_shortcuts = gtk::EventControllerKey::new();
        settings_shortcuts.set_propagation_phase(gtk::PropagationPhase::Capture);
        settings_shortcuts.connect_key_pressed({
            let dark_theme = dark_theme.clone();
            let light_theme = light_theme.clone();
            let apply = apply.clone();
            move |_, key, _, modifiers| {
                if modifiers.contains(gdk::ModifierType::ALT_MASK) {
                    match key {
                        gdk::Key::d => dark_theme.set_active(true),
                        gdk::Key::l => light_theme.set_active(true),
                        _ => return glib::Propagation::Proceed,
                    }
                    return glib::Propagation::Stop;
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
        dialog.add_controller(settings_shortcuts);
        dialog.connect_close_request({
            let weak = Rc::downgrade(controller);
            move |_| {
                if let Some(controller) = weak.upgrade() {
                    controller.borrow_mut().settings_dialog = None;
                }
                glib::Propagation::Proceed
            }
        });
        controller.borrow_mut().settings_dialog = Some(dialog.clone());
        dialog.present();
        server.grab_focus();
    }

    fn apply_preferences(&mut self, config: &ApiConfig, theme: ThemePreference) {
        self.connection_config = config.clone();
        self.theme = theme;
        self.persisted.connection = ConnectionSettings {
            server: config.base_url.clone(),
            username: config.username.clone(),
            cloudflare_access: config.cloudflare_access.is_some(),
        };
        self.credential_warning = None;
        self.persisted.theme = theme;
        apply_theme(&self.css_provider, theme);
        self.persist_state();
    }

    fn switch_connection(
        controller: &Rc<RefCell<Self>>,
        config: ApiConfig,
        theme: ThemePreference,
        api: ApiHandle,
        events: Receiver<UiEvent>,
        server_key: String,
    ) {
        let (old_dialogs, old_new_session, generation) = {
            let mut this = controller.borrow_mut();
            this.persist_state();
            this.events.close();
            this.connection_generation += 1;
            let generation = this.connection_generation;
            let old_dialogs = std::mem::take(&mut this.dialogs)
                .into_values()
                .collect::<Vec<_>>();
            let old_new_session = this.new_session_dialog.take();

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
            this.connection_config = config.clone();
            this.theme = theme;
            this.persisted.connection = ConnectionSettings {
                server: config.base_url,
                username: config.username,
                cloudflare_access: config.cloudflare_access.is_some(),
            };
            this.credential_warning = None;
            this.persisted.theme = theme;
            apply_theme(&this.css_provider, theme);
            this.state = restored_state(server_state);
            this.rendered_session = None;
            this.rendered_rows.clear();
            this.transcript_at_bottom = true;
            this.transcript_scroll_generation += 1;
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
            this.connected_once = false;
            this.event_connected = false;
            this.widgets.status.remove_css_class("error");
            this.widgets
                .status
                .set_label(&format!("Connecting to {server_key}"));
            this.widgets.status.set_tooltip_text(Some(&server_key));
            this.persist_state();
            (old_dialogs, old_new_session, generation)
        };

        for dialog in old_dialogs {
            dialog.hide();
        }
        if let Some(dialog) = old_new_session {
            dialog.hide();
        }
        Self::refresh_all(controller);
        start_event_loop(controller, events, generation);
        controller.borrow().api.send(Command::Bootstrap);
    }

    fn show_new_session(controller: &Rc<RefCell<Self>>) {
        if let Some(dialog) = controller.borrow().new_session_dialog.clone() {
            dialog.present();
            return;
        }
        let this = controller.borrow();
        let dialog = gtk::Window::builder()
            .title("New session")
            .transient_for(&this.widgets.window)
            .modal(true)
            .default_width(560)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.set_margin_top(18);
        root.set_margin_bottom(18);
        root.set_margin_start(18);
        root.set_margin_end(18);
        let directory_label = gtk::Label::new(Some("Directory on the OpenCode server"));
        directory_label.set_xalign(0.0);
        let directory = gtk::Entry::new();
        directory.set_activates_default(true);
        directory.set_placeholder_text(Some("/path/to/project"));
        if let Some(active_directory) = this.active_directory() {
            directory.set_text(&active_directory);
        } else if let Some(project) = this.state.projects.first() {
            directory.set_text(&project.worktree);
        }
        let known_directories: Vec<_> = this
            .state
            .projects
            .iter()
            .map(|project| project.worktree.as_str())
            .collect();
        let directory_store = gtk::StringList::new(&known_directories);
        let directory_dropdown = gtk::DropDown::new(Some(directory_store), None::<gtk::Expression>);
        directory_dropdown.set_visible(!known_directories.is_empty());
        directory_dropdown.connect_selected_notify({
            let directory = directory.clone();
            move |dropdown| {
                let Some(item) = dropdown.selected_item().and_downcast::<gtk::StringObject>()
                else {
                    return;
                };
                directory.set_text(&item.string());
            }
        });
        let title_label = gtk::Label::new(Some("Title (optional)"));
        title_label.set_xalign(0.0);
        let title = gtk::Entry::new();
        title.set_activates_default(true);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        let create = gtk::Button::with_label("Create");
        create.add_css_class("suggested-action");
        actions.append(&cancel);
        actions.append(&create);
        root.append(&directory_label);
        root.append(&directory);
        root.append(&directory_dropdown);
        root.append(&title_label);
        root.append(&title);
        root.append(&actions);
        dialog.set_child(Some(&root));
        dialog.set_default_widget(Some(&create));
        directory_label.set_mnemonic_widget(Some(&directory));
        title_label.set_mnemonic_widget(Some(&title));
        drop(this);

        cancel.connect_clicked({
            let dialog = dialog.clone();
            move |_| dialog.close()
        });
        create.connect_clicked({
            let weak = Rc::downgrade(controller);
            let dialog = dialog.clone();
            let directory = directory.clone();
            let title = title.clone();
            move |_| {
                let value = directory.text().trim().to_owned();
                if value.is_empty() {
                    directory.add_css_class("error");
                    return;
                }
                if let Some(controller) = weak.upgrade() {
                    let (api, request_id) = {
                        let mut this = controller.borrow_mut();
                        this.next_session_request_id += 1;
                        let request_id = this.next_session_request_id;
                        this.pending_session_request = Some(request_id);
                        (this.api.clone(), request_id)
                    };
                    api.send(Command::CreateSession {
                        request_id,
                        directory: value,
                        title: (!title.text().trim().is_empty())
                            .then(|| title.text().trim().to_owned()),
                    });
                    dialog.set_sensitive(false);
                }
            }
        });
        dialog.connect_close_request({
            let weak = Rc::downgrade(controller);
            move |_| {
                if let Some(controller) = weak.upgrade() {
                    controller.borrow_mut().new_session_dialog = None;
                }
                glib::Propagation::Proceed
            }
        });
        controller.borrow_mut().new_session_dialog = Some(dialog.clone());
        dialog.present();
        directory.grab_focus();
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

        let parent = controller.borrow().widgets.window.clone();
        let dialog = gtk::Window::builder()
            .title("Permission required")
            .transient_for(&parent)
            .modal(true)
            .default_width(620)
            .default_height(480)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_top(18);
        root.set_margin_bottom(18);
        root.set_margin_start(18);
        root.set_margin_end(18);
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
        dialog.set_child(Some(&root));

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
        dialog.connect_close_request({
            let weak = Rc::downgrade(controller);
            let request_id = request_id.clone();
            let directory = directory.clone();
            move |_| {
                let waiting = submit_request(
                    &weak,
                    &request_id,
                    Command::ReplyPermission {
                        request_id: request_id.clone(),
                        directory: directory.clone(),
                        reply: "reject".to_owned(),
                    },
                );
                if waiting {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        controller
            .borrow_mut()
            .dialogs
            .insert(request_id, dialog.clone());
        dialog.present();
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

        let parent = controller.borrow().widgets.window.clone();
        let dialog = gtk::Window::builder()
            .title("OpenCode needs your input")
            .transient_for(&parent)
            .modal(true)
            .default_width(680)
            .default_height(560)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_top(18);
        root.set_margin_bottom(18);
        root.set_margin_start(18);
        root.set_margin_end(18);
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
        dialog.set_child(Some(&root));

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
        dialog.connect_close_request({
            let weak = Rc::downgrade(controller);
            let request_id = request_id.clone();
            let directory = directory.clone();
            move |_| {
                let waiting = submit_request(
                    &weak,
                    &request_id,
                    Command::RejectQuestion {
                        request_id: request_id.clone(),
                        directory: directory.clone(),
                    },
                );
                if waiting {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        controller
            .borrow_mut()
            .dialogs
            .insert(request_id, dialog.clone());
        dialog.present();
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
        self.state.server_busy.remove(id);
        self.state.abort_requested.remove(id);
        self.state.drafts.remove(id);
        self.state.pending_prompts.remove(id);
        self.message_load_errors.remove(id);
        if self.state.active.as_deref() == Some(id) {
            self.state.active = self.state.tabs.last().cloned();
        }
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

fn populate_session_list(
    list: &gtk::ListBox,
    sessions: &[Session],
    query: &str,
    controller: Weak<RefCell<Controller>>,
    dialog: gtk::Window,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let query = query.to_lowercase();
    let mut shown = 0;
    let mut truncated = false;
    for session in sessions.iter().filter(|session| {
        query.is_empty()
            || session.title.to_lowercase().contains(&query)
            || session.directory.to_lowercase().contains(&query)
    }) {
        if shown == SESSION_PICKER_LIMIT {
            truncated = true;
            break;
        }
        shown += 1;
        let button = gtk::Button::new();
        button.add_css_class("session-picker-row");
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 3);
        let title = gtk::Label::new(Some(&session.title));
        title.set_xalign(0.0);
        title.add_css_class("session-picker-title");
        let path = gtk::Label::new(Some(&session.directory));
        path.set_xalign(0.0);
        path.set_ellipsize(pango::EllipsizeMode::Middle);
        path.add_css_class("session-picker-path");
        labels.append(&title);
        labels.append(&path);
        button.set_child(Some(&labels));
        let id = session.id.clone();
        let weak = controller.clone();
        let dialog = dialog.clone();
        button.connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                Controller::open_tab(&controller, &id);
            }
            dialog.close();
        });
        list.append(&button);
    }
    if shown == 0 {
        list.append(&gtk::Label::new(Some("No matching sessions")));
    } else if truncated {
        let hint = gtk::Label::new(Some(
            "More sessions match. Refine the search to narrow the list.",
        ));
        hint.set_wrap(true);
        hint.set_margin_top(12);
        hint.set_margin_bottom(12);
        list.append(&hint);
    }
}

fn replace_string_list(model: &gtk::StringList, values: &[&str]) {
    model.splice(0, model.n_items(), values);
}

fn sync_string_list(model: &gtk::StringList, old: &mut Vec<String>, new: Vec<String>) {
    let common = old.len().min(new.len());
    for index in 0..common {
        if old[index] != new[index] {
            model.splice(index as u32, 1, &[new[index].as_str()]);
        }
    }
    if new.len() > old.len() {
        let additions: Vec<_> = new[old.len()..].iter().map(String::as_str).collect();
        model.splice(old.len() as u32, 0, &additions);
    } else if old.len() > new.len() {
        model.splice(new.len() as u32, (old.len() - new.len()) as u32, &[]);
    }
    *old = new;
}

fn sync_transcript_list(
    model: &gtk::StringList,
    rendered_session: &mut Option<String>,
    rendered_rows: &mut Vec<String>,
    active_session: Option<&str>,
    new_rows: Vec<String>,
) {
    if transcript_session_changed(rendered_session.as_deref(), active_session) {
        let values: Vec<_> = new_rows.iter().map(String::as_str).collect();
        replace_string_list(model, &values);
        *rendered_session = active_session.map(str::to_owned);
        *rendered_rows = new_rows;
        return;
    }
    sync_string_list(model, rendered_rows, new_rows);
}

fn transcript_session_changed(
    rendered_session: Option<&str>,
    active_session: Option<&str>,
) -> bool {
    rendered_session != active_session
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

fn clamp_adjustment(adjustment: &gtk::Adjustment, value: f64) -> f64 {
    let lower = adjustment.lower();
    let upper = (adjustment.upper() - adjustment.page_size()).max(lower);
    value.clamp(lower, upper)
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

fn status_transitioned_to_idle(previous: Option<RunStatus>, current: RunStatus) -> bool {
    previous == Some(RunStatus::Busy) && current == RunStatus::Idle
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn buffer_text(buffer: &gtk::TextBuffer) -> String {
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string()
}

fn install_css(theme: ThemePreference) -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    apply_theme(&provider, theme);
    provider
}

fn apply_theme(provider: &gtk::CssProvider, theme: ThemePreference) {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_property(
            "gtk-application-prefer-dark-theme",
            theme == ThemePreference::Dark,
        );
    }
    provider.load_from_data(match theme {
        ThemePreference::Dark => include_str!("style.css"),
        ThemePreference::Light => {
            concat!(
                include_str!("style.css"),
                "\n",
                include_str!("style-light.css")
            )
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn idle_notifications_only_follow_an_observed_busy_state() {
        assert!(status_transitioned_to_idle(
            Some(RunStatus::Busy),
            RunStatus::Idle
        ));
        assert!(!status_transitioned_to_idle(None, RunStatus::Idle));
        assert!(!status_transitioned_to_idle(
            Some(RunStatus::Idle),
            RunStatus::Idle
        ));
        assert!(!status_transitioned_to_idle(
            Some(RunStatus::Busy),
            RunStatus::Busy
        ));
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
}
