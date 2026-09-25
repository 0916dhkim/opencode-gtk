//! Pending permission requests and forms: recovery results, event changes,
//! and the pure display rules the UI builds its prompt and notice from.

use std::collections::HashMap;

use serde_json::Value;

use crate::protocol::{
    EventKind, FormInfo, JsonMap, PermissionRequest, PermissionSource, GLOBAL_FORM_OWNER,
};

/// Shortcut that cancels the form shown in the notice.
pub const CANCEL_FORM_SHORTCUT: &str = "Ctrl+Shift+X";

/// A form waiting for input. `directory` is its location, needed to cancel a
/// [`GLOBAL_FORM_OWNER`] form.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingForm {
    pub form: FormInfo,
    pub directory: Option<String>,
}

/// One request recovered from the pending lists (bootstrap, reconnect or a
/// reconciliation). Fed through the same path as the live events.
#[derive(Clone, Debug, PartialEq)]
pub enum PendingRequest {
    Permission {
        directory: String,
        request: PermissionRequest,
    },
    Form(PendingForm),
}

impl PendingRequest {
    pub fn id(&self) -> &str {
        match self {
            Self::Permission { request, .. } => &request.id,
            Self::Form(pending) => &pending.form.id,
        }
    }
}

/// The pending lists of every queried location. `complete` is true only
/// when every list was fetched; otherwise open prompts must be kept.
#[derive(Debug, Default)]
pub struct PendingSnapshot {
    pub requests: Vec<PendingRequest>,
    pub complete: bool,
    pub warnings: Vec<String>,
}

/// What a live event changes about pending requests.
#[derive(Clone, Debug, PartialEq)]
pub enum PendingChange {
    Permission {
        directory: Option<String>,
        request: PermissionRequest,
    },
    Form(PendingForm),
    /// A permission was replied to, or a form answered or cancelled.
    Resolved(String),
}

pub fn pending_change(kind: &EventKind, directory: Option<&str>) -> Option<PendingChange> {
    let directory = directory.filter(|d| !d.is_empty()).map(str::to_owned);
    match kind {
        EventKind::PermissionAsked(request) => Some(PendingChange::Permission {
            directory,
            request: request.clone(),
        }),
        EventKind::PermissionReplied(replied) => {
            Some(PendingChange::Resolved(replied.request_id.clone()))
        }
        EventKind::FormCreated(created) => Some(PendingChange::Form(PendingForm {
            form: created.form.clone(),
            directory,
        })),
        EventKind::FormReplied(settled) | EventKind::FormCancelled(settled) => {
            Some(PendingChange::Resolved(settled.id.clone()))
        }
        _ => None,
    }
}

/// `(child, parent)` when a `subagent` tool reports its child session in
/// its metadata, so the child's requests can be attributed to the parent.
pub fn subagent_child(kind: &EventKind) -> Option<(String, String)> {
    let (parent, metadata) = match kind {
        EventKind::ToolProgress(progress) => (&progress.session_id, Some(&progress.metadata)),
        EventKind::ToolSuccess(success) => (&success.session_id, success.metadata.as_ref()),
        _ => return None,
    };
    let child = metadata?.get("sessionID")?.as_str()?;
    (!child.is_empty() && child != parent).then(|| (child.to_owned(), parent.clone()))
}

/// The session whose composer a permission prompt replaces, or `None` to show
/// it whatever session is active. A child session's request is shown with its
/// parent when the parent is known; a session outside the root list is shown
/// everywhere, since the parent run is blocked until someone answers.
pub fn permission_scope(
    session_id: &str,
    is_root: impl Fn(&str) -> bool,
    parents: &HashMap<String, String>,
) -> Option<String> {
    if is_root(session_id) {
        return Some(session_id.to_owned());
    }
    parents
        .get(session_id)
        .filter(|parent| is_root(parent))
        .cloned()
}

/// The `save` patterns "Always allow" would remember, or `None` when the
/// request does not offer "always".
pub fn always_patterns(request: &PermissionRequest) -> Option<String> {
    request
        .offers_always()
        .then(|| request.save.as_deref().unwrap_or_default().join("\n"))
}

pub fn source_text(request: &PermissionRequest) -> Option<String> {
    match &request.source {
        Some(PermissionSource::Tool { id, .. }) if !id.is_empty() => {
            Some(format!("Tool call {id}"))
        }
        _ => None,
    }
}

/// Tool metadata as pretty JSON, or `None` when there is nothing to show.
/// Keys named like credentials or provider settings are dropped at any depth.
pub fn metadata_text(metadata: Option<&JsonMap>) -> Option<String> {
    let mut value = Value::Object(metadata?.clone());
    strip_sensitive(&mut value);
    match &value {
        Value::Object(map) if map.is_empty() => None,
        _ => serde_json::to_string_pretty(&value).ok(),
    }
}

fn strip_sensitive(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| {
                let key = key.to_ascii_lowercase();
                key != "apikey" && key != "settings"
            });
            map.values_mut().for_each(strip_sensitive);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_sensitive),
        _ => {}
    }
}

/// The form to cancel from the notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelTarget {
    pub form_id: String,
    pub session_id: String,
    pub directory: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormNotice {
    pub text: String,
    pub tooltip: String,
    /// `None` when every waiting form belongs to another session.
    pub cancel: Option<CancelTarget>,
}

/// Pending forms in arrival order.
#[derive(Clone, Debug, Default)]
pub struct Forms {
    items: Vec<PendingForm>,
}

impl Forms {
    /// Adds a form, or refreshes a known one (keeping a known location).
    pub fn upsert(&mut self, pending: PendingForm) {
        match self
            .items
            .iter_mut()
            .find(|item| item.form.id == pending.form.id)
        {
            Some(existing) => {
                let directory = pending.directory.or(existing.directory.take());
                *existing = PendingForm {
                    form: pending.form,
                    directory,
                };
            }
            None => self.items.push(pending),
        }
    }

    pub fn remove(&mut self, form_id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.form.id != form_id);
        self.items.len() != before
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(|item| item.form.id.as_str())
    }

    pub fn directories(&self) -> impl Iterator<Item = &str> {
        self.items
            .iter()
            .filter_map(|item| item.directory.as_deref())
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// `"global"` forms (MCP elicitation), the active session's forms and
    /// those of its known child sessions.
    pub fn visible(
        &self,
        active: Option<&str>,
        parents: &HashMap<String, String>,
    ) -> Vec<&PendingForm> {
        self.items
            .iter()
            .filter(|item| form_is_visible(&item.form, active, parents))
            .collect()
    }

    /// The one-line notice, or `None` when no form is waiting. Forms of other
    /// sessions only add a count; the active session's (or a global) form
    /// is the one Cancel acts on.
    pub fn notice(
        &self,
        active: Option<&str>,
        parents: &HashMap<String, String>,
    ) -> Option<FormNotice> {
        let visible = self.visible(active, parents);
        let elsewhere = self.items.len() - visible.len();
        let Some(first) = visible.first() else {
            return (elsewhere > 0).then(|| FormNotice {
                text: if elsewhere == 1 {
                    "A form is waiting for input in another session".to_owned()
                } else {
                    format!("{elsewhere} forms are waiting for input in other sessions")
                },
                tooltip: "Open the server's web UI to answer, or switch to the session to cancel"
                    .to_owned(),
                cancel: None,
            });
        };
        let mut text = format!("{} is waiting for input", form_title(&first.form));
        if visible.len() > 1 {
            text.push_str(&format!(" (+{} more)", visible.len() - 1));
        }
        if elsewhere > 0 {
            text.push_str(&format!(" · {elsewhere} in other sessions"));
        }
        let requester = if first.form.is_global() {
            "Requested by an MCP server"
        } else {
            "Requested by the agent"
        };
        Some(FormNotice {
            text,
            tooltip: format!(
                "{requester}. GTK cannot fill in forms: answer it in the server's web UI, \
                 or cancel it ({CANCEL_FORM_SHORTCUT})."
            ),
            cancel: Some(CancelTarget {
                form_id: first.form.id.clone(),
                session_id: first.form.session_id.clone(),
                directory: first.directory.clone(),
            }),
        })
    }
}

fn form_is_visible(
    form: &FormInfo,
    active: Option<&str>,
    parents: &HashMap<String, String>,
) -> bool {
    if form.session_id == GLOBAL_FORM_OWNER {
        return true;
    }
    let Some(active) = active else {
        return false;
    };
    form.session_id == active || parents.get(&form.session_id).is_some_and(|p| p == active)
}

fn form_title(form: &FormInfo) -> &str {
    match form.title.trim() {
        "" => "A form",
        title => title,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::protocol::{self, Event};

    fn request(value: Value) -> PermissionRequest {
        serde_json::from_value(value).unwrap()
    }

    fn form(id: &str, session_id: &str, title: &str) -> PendingForm {
        PendingForm {
            form: serde_json::from_value(
                json!({ "id": id, "sessionID": session_id, "title": title }),
            )
            .unwrap(),
            directory: Some("/repo".into()),
        }
    }

    #[test]
    fn always_is_offered_only_with_save_patterns() {
        let with_save = request(json!({
            "id": "per_1", "sessionID": "ses_a", "action": "shell",
            "resources": ["echo hi"], "save": ["echo *", "ls *"]
        }));
        assert_eq!(always_patterns(&with_save).as_deref(), Some("echo *\nls *"));
        for save in [json!(null), json!([])] {
            let without = request(json!({ "id": "per_2", "sessionID": "ses_a", "save": save }));
            assert!(!without.offers_always());
            assert_eq!(always_patterns(&without), None);
        }
    }

    #[test]
    fn source_and_metadata_are_shown_only_when_present() {
        let tool = request(json!({
            "id": "per_1", "sessionID": "ses_a",
            "source": { "type": "tool", "messageID": "msg_1", "id": "call_shell" }
        }));
        assert_eq!(source_text(&tool).as_deref(), Some("Tool call call_shell"));
        assert_eq!(
            source_text(&request(json!({ "id": "per_2", "sessionID": "s" }))),
            None
        );

        assert_eq!(metadata_text(None), None);
        assert_eq!(metadata_text(Some(&JsonMap::new())), None);
        let sensitive = json!({ "apiKey": "sk-secret", "Settings": { "x": 1 } });
        assert_eq!(metadata_text(sensitive.as_object()), None);
        let nested = json!({ "command": "ls", "provider": { "apikey": "sk", "name": "p" } });
        let text = metadata_text(nested.as_object()).unwrap();
        assert!(text.contains("\"command\": \"ls\""), "{text}");
        assert!(text.contains("\"name\": \"p\""), "{text}");
        assert!(
            !text.to_ascii_lowercase().contains("apikey") && !text.contains("sk"),
            "{text}"
        );
    }

    #[test]
    fn child_requests_follow_a_known_root_parent_or_show_everywhere() {
        let roots = ["ses_root"];
        let is_root = |id: &str| roots.contains(&id);
        let parents = HashMap::from([
            ("ses_child".to_owned(), "ses_root".to_owned()),
            ("ses_grandchild".to_owned(), "ses_child".to_owned()),
        ]);
        assert_eq!(
            permission_scope("ses_root", is_root, &parents).as_deref(),
            Some("ses_root")
        );
        assert_eq!(
            permission_scope("ses_child", is_root, &parents).as_deref(),
            Some("ses_root")
        );
        assert_eq!(permission_scope("ses_grandchild", is_root, &parents), None);
        assert_eq!(permission_scope("ses_unknown", is_root, &parents), None);
    }

    #[test]
    fn the_notice_shows_active_and_global_forms_and_counts_the_rest() {
        let parents = HashMap::from([("ses_child".to_owned(), "ses_a".to_owned())]);
        let mut forms = Forms::default();
        assert_eq!(forms.notice(Some("ses_a"), &parents), None);

        forms.upsert(form("frm_b", "ses_b", "Deploy target"));
        let elsewhere = forms.notice(Some("ses_a"), &parents).unwrap();
        assert_eq!(
            elsewhere.text,
            "A form is waiting for input in another session"
        );
        assert_eq!(elsewhere.cancel, None);

        forms.upsert(form("frm_a", "ses_a", "  "));
        forms.upsert(form("frm_global", GLOBAL_FORM_OWNER, "Slack login"));
        forms.upsert(form("frm_child", "ses_child", "Pick a branch"));
        let ids = |active| -> Vec<&str> {
            forms
                .visible(active, &parents)
                .iter()
                .map(|item| item.form.id.as_str())
                .collect()
        };
        assert_eq!(ids(Some("ses_a")), ["frm_a", "frm_global", "frm_child"]);
        assert_eq!(ids(Some("ses_b")), ["frm_b", "frm_global"]);
        assert_eq!(ids(None), ["frm_global"]);

        let notice = forms.notice(Some("ses_a"), &parents).unwrap();
        assert_eq!(
            notice.text,
            "A form is waiting for input (+2 more) · 1 in other sessions"
        );
        assert_eq!(
            notice.cancel,
            Some(CancelTarget {
                form_id: "frm_a".into(),
                session_id: "ses_a".into(),
                directory: Some("/repo".into()),
            })
        );
        assert!(notice.tooltip.contains(CANCEL_FORM_SHORTCUT));

        assert!(forms.remove("frm_a"));
        assert!(!forms.remove("frm_a"));
        let notice = forms.notice(None, &parents).unwrap();
        assert_eq!(
            notice.text,
            "Slack login is waiting for input · 2 in other sessions"
        );
        assert!(notice.tooltip.starts_with("Requested by an MCP server"));
        assert_eq!(
            notice.cancel.map(|target| target.session_id),
            Some(GLOBAL_FORM_OWNER.to_owned())
        );
    }

    #[test]
    fn upsert_keeps_a_known_location() {
        let mut forms = Forms::default();
        forms.upsert(form("frm_1", GLOBAL_FORM_OWNER, "Old"));
        let mut again = form("frm_1", GLOBAL_FORM_OWNER, "New");
        again.directory = None;
        forms.upsert(again);
        assert_eq!(forms.ids().collect::<Vec<_>>(), ["frm_1"]);
        let notice = forms.notice(None, &HashMap::new()).unwrap();
        assert_eq!(notice.text, "New is waiting for input");
        assert_eq!(notice.cancel.unwrap().directory.as_deref(), Some("/repo"));
    }

    fn scenario(name: &str) -> Vec<(Event, EventKind)> {
        let path = format!(
            "{}/tests/fixtures/v2-2.0.8/events/{name}.jsonl",
            env!("CARGO_MANIFEST_DIR")
        );
        fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{path}: {error}"))
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let event: Event = serde_json::from_str(line).unwrap();
                let kind = protocol::decode_event(&event);
                (event, kind)
            })
            .collect()
    }

    /// Replays a captured scenario through the same reducers the UI uses,
    /// returning the pending state after every event.
    #[derive(Default, Clone)]
    struct Pending {
        permissions: HashMap<String, (Option<String>, PermissionRequest)>,
        forms: Forms,
        parents: HashMap<String, String>,
    }

    fn replay(name: &str) -> Vec<Pending> {
        let mut state = Pending::default();
        let mut history = Vec::new();
        for (event, kind) in scenario(name) {
            if let Some((child, parent)) = subagent_child(&kind) {
                state.parents.insert(child, parent);
            }
            match pending_change(&kind, event.directory()) {
                Some(PendingChange::Permission { directory, request }) => {
                    state
                        .permissions
                        .insert(request.id.clone(), (directory, request));
                }
                Some(PendingChange::Form(form)) => state.forms.upsert(form),
                Some(PendingChange::Resolved(id)) => {
                    state.permissions.remove(&id);
                    state.forms.remove(&id);
                }
                None => {}
            }
            history.push(state.clone());
        }
        history
    }

    #[test]
    fn permission_scenarios_ask_then_resolve() {
        for name in ["permission", "subagent-permission"] {
            let history = replay(name);
            let asked = history
                .iter()
                .find(|state| !state.permissions.is_empty())
                .unwrap_or_else(|| panic!("{name}: no permission.asked"));
            let (directory, request) = asked.permissions.values().next().unwrap();
            assert_eq!(directory.as_deref(), Some("/state/workspace"), "{name}");
            assert_eq!(request.action, "shell");
            assert_eq!(request.resources, ["echo permission-probe"]);
            assert_eq!(always_patterns(request).as_deref(), Some("echo *"));
            assert_eq!(
                source_text(request).as_deref(),
                Some("Tool call call_mock_shell")
            );
            assert!(
                history.last().unwrap().permissions.is_empty(),
                "{name}: replied"
            );
        }
    }

    #[test]
    fn a_child_permission_is_attributed_to_its_parent() {
        let history = replay("subagent-permission");
        let asked = history
            .iter()
            .find(|state| !state.permissions.is_empty())
            .unwrap();
        let (_, request) = asked.permissions.values().next().unwrap();
        assert_eq!(request.session_id, "ses_f2a743737ffe9Gxk6NjdQqWkMW");
        let parent = "ses_f2a743750ffev3c11LrAwbuAQk";
        assert_eq!(
            asked.parents.get(&request.session_id).map(String::as_str),
            Some(parent)
        );
        assert_eq!(
            permission_scope(&request.session_id, |id| id == parent, &asked.parents).as_deref(),
            Some(parent)
        );
        assert_eq!(
            permission_scope(&request.session_id, |_| false, &asked.parents),
            None,
            "an unknown parent shows the prompt everywhere"
        );
    }

    #[test]
    fn the_form_scenario_creates_then_cancels() {
        let history = replay("form");
        let created = history
            .iter()
            .find(|state| state.forms.ids().next().is_some())
            .expect("form.created");
        let owner = "ses_f2a747fb3ffeqHy7NW2TISzm9M";
        let notice = created.forms.notice(Some(owner), &HashMap::new()).unwrap();
        assert_eq!(notice.text, "Harness form is waiting for input");
        assert_eq!(
            notice.cancel,
            Some(CancelTarget {
                form_id: "frm_0d58bede7001Oe4MviTBLpPVWZ".into(),
                session_id: owner.into(),
                directory: Some("/state/workspace".into()),
            })
        );
        assert!(
            history.last().unwrap().forms.ids().next().is_none(),
            "form.cancelled"
        );
    }
}
