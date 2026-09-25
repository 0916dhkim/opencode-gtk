//! Running background jobs for the sidebar's "Background" section: running
//! child sessions (subagents) and running shell commands, of every session.
//!
//! OpenCode 2.0.8 has no job list. Subagents are the sessions `GET
//! /api/session/active` reports running that have a `parentID`, kept live by
//! `session.execution.*`. Shells come from `GET /api/shell` per location
//! (the locations the pending lists already cover, so no new location is
//! started) and `shell.created|exited|deleted`. Foreground (blocking)
//! subagents and shells are jobs too: anything running besides a chat turn.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::{
    model::{Session, SessionChange},
    protocol::{self, EventKind, ShellStatus},
};

/// Elapsed times refresh at most this often, while the section is shown.
pub const ELAPSED_REFRESH_SECONDS: u32 = 30;
const MAX_PARENT_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Subagent,
    Shell,
}

impl JobKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Subagent => "subagent",
            Self::Shell => "shell",
        }
    }
}

/// A running shell command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellJob {
    pub id: String,
    /// The location it was listed or announced in.
    pub directory: String,
    pub command: String,
    /// `metadata.sessionID`: the session whose tool started it, possibly a
    /// child session.
    pub session_id: Option<String>,
    pub started: u64,
}

impl ShellJob {
    /// `None` unless the command is still running.
    pub fn running(info: &protocol::ShellInfo, directory: &str) -> Option<Self> {
        (info.status == ShellStatus::Running).then(|| Self {
            id: info.id.clone(),
            directory: directory.to_owned(),
            command: info.command.clone(),
            session_id: info.session_id().map(str::to_owned),
            started: u64::try_from(info.time.started).unwrap_or(0),
        })
    }
}

/// The running shells of the locations one refresh listed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShellSnapshot {
    pub shells: Vec<ShellJob>,
    /// Every location that was listed…
    pub queried: BTreeSet<String>,
    /// …and those whose list loaded, plus the directories the server
    /// resolved them to.
    pub covered: HashSet<String>,
    pub warnings: Vec<String>,
}

/// What a live event changes about the jobs.
#[derive(Clone, Debug, PartialEq)]
pub enum JobEvent {
    /// `session.execution.started`.
    Started(String),
    /// `session.execution.succeeded|failed|interrupted`.
    Ended(String),
    /// `session.created` of a child session: its title and parent.
    ChildCreated(Session),
    Renamed {
        id: String,
        title: String,
    },
    Deleted(String),
    ShellCreated(ShellJob),
    /// `shell.exited` or `shell.deleted`.
    ShellRemoved(String),
}

/// `directory` is the event's location, if any.
pub fn job_event(
    event: &protocol::Event,
    kind: &EventKind,
    directory: Option<&str>,
) -> Option<JobEvent> {
    match kind {
        EventKind::ExecutionStarted(data) => Some(JobEvent::Started(data.session_id.clone())),
        EventKind::ExecutionSucceeded(data) => Some(JobEvent::Ended(data.session_id.clone())),
        EventKind::ExecutionFailed(data) => Some(JobEvent::Ended(data.session_id.clone())),
        EventKind::ExecutionInterrupted(data) => Some(JobEvent::Ended(data.session_id.clone())),
        EventKind::SessionCreated(_)
        | EventKind::SessionRenamed(_)
        | EventKind::SessionDeleted(_) => match SessionChange::from_kind(event, kind)? {
            SessionChange::Created(session) if session.parent_id.is_some() => {
                Some(JobEvent::ChildCreated(session))
            }
            SessionChange::Renamed { id, title } => Some(JobEvent::Renamed { id, title }),
            SessionChange::Deleted(id) => Some(JobEvent::Deleted(id)),
            _ => None,
        },
        EventKind::ShellCreated(data) => {
            let directory = directory
                .filter(|directory| !directory.is_empty())
                .unwrap_or(&data.info.cwd);
            ShellJob::running(&data.info, directory).map(JobEvent::ShellCreated)
        }
        EventKind::ShellExited(data) if data.status != ShellStatus::Running => {
            Some(JobEvent::ShellRemoved(data.id.clone()))
        }
        EventKind::ShellDeleted(data) => Some(JobEvent::ShellRemoved(data.id.clone())),
        _ => None,
    }
}

/// What the jobs list needs from the rest of the client.
pub struct Context<'a> {
    /// The root-session list.
    pub roots: &'a [Session],
    /// The locations whose shells a refresh lists
    /// ([`crate::pending::pending_directories`]).
    pub directories: &'a [String],
}

impl Context<'_> {
    fn is_root(&self, id: &str) -> bool {
        self.roots
            .iter()
            .any(|session| session.id == id && session.parent_id.is_none())
    }
}

/// One row of the section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRow {
    /// Child session or shell ID.
    pub id: String,
    pub kind: JobKind,
    pub title: String,
    /// Title of the session that started the job, when known.
    pub owner: Option<String>,
    /// The root session whose tab a click activates; `None` when unknown.
    pub root: Option<String>,
    /// Start time in ms; 0 when unknown.
    pub started: u64,
}

impl JobRow {
    /// `<kind> · <owner> · <elapsed>`; unknown parts are left out.
    pub fn subtitle(&self, now: u64) -> String {
        let (label, elapsed) = self.subtitle_parts(now);
        match elapsed {
            Some(elapsed) => format!("{label} · {elapsed}"),
            None => label,
        }
    }

    /// `<kind> · <owner>` and the elapsed time, shown apart so that a long
    /// owner title ellipsizes without hiding the time.
    pub fn subtitle_parts(&self, now: u64) -> (String, Option<String>) {
        let label = match &self.owner {
            Some(owner) => format!("{} · {owner}", self.kind.label()),
            None => self.kind.label().to_owned(),
        };
        let elapsed = (self.started > 0).then(|| format_elapsed(now.saturating_sub(self.started)));
        (label, elapsed)
    }
}

/// `<1m`, `4m`, `1h 5m`, `2d 3h`.
pub fn format_elapsed(ms: u64) -> String {
    let minutes = ms / 60_000;
    let (days, hours, minutes) = (minutes / 1_440, minutes / 60 % 24, minutes % 60);
    match (days, hours, minutes) {
        (0, 0, 0) => "<1m".to_owned(),
        (0, 0, minutes) => format!("{minutes}m"),
        (0, hours, 0) => format!("{hours}h"),
        (0, hours, minutes) => format!("{hours}h {minutes}m"),
        (days, 0, _) => format!("{days}d"),
        (days, hours, _) => format!("{days}d {hours}h"),
    }
}

/// A shell's title: the first non-blank line of its command.
fn command_title(command: &str) -> String {
    command
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("shell")
        .to_owned()
}

#[derive(Debug, Default)]
pub struct Jobs {
    /// Running sessions outside the root list. A root session that is not
    /// listed (e.g. archived) stays here only until its info shows it has
    /// no parent.
    running: BTreeSet<String>,
    /// Sessions outside the root list (children and their parents), from
    /// `GET /api/session/{id}` or `session.created`.
    sessions: HashMap<String, Session>,
    /// Session infos requested and not answered yet.
    requested: HashSet<String>,
    /// Sessions whose info failed to load; retried after the next refresh.
    unresolved: HashSet<String>,
    shells: BTreeMap<String, ShellJob>,
    /// Locations the last refresh covered, as the server named them.
    locations: HashSet<String>,
    /// Events seen while a refresh is in flight, replayed onto it.
    journal: Option<Vec<JobEvent>>,
}

impl Jobs {
    /// A connection switch starts over.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// A refresh (bootstrap) started: events from now on are replayed onto
    /// its snapshot.
    pub fn begin_refresh(&mut self) {
        self.journal = Some(Vec::new());
    }

    /// The refresh failed; nothing will be replayed.
    pub fn abandon_refresh(&mut self) {
        self.journal = None;
    }

    /// Applies a live event; returns whether the rows may have changed.
    pub fn apply_event(&mut self, event: JobEvent, context: &Context) -> bool {
        if let Some(journal) = &mut self.journal {
            journal.push(event.clone());
        }
        self.apply(event, context)
    }

    fn apply(&mut self, event: JobEvent, context: &Context) -> bool {
        match event {
            JobEvent::Started(id) => {
                !context.is_root(&id) && !self.is_known_root(&id) && self.running.insert(id)
            }
            JobEvent::Ended(id) => self.running.remove(&id),
            JobEvent::ChildCreated(session) => {
                let running = self.running.contains(&session.id);
                self.requested.remove(&session.id);
                self.sessions.insert(session.id.clone(), session);
                running
            }
            JobEvent::Renamed { id, title } => match self.sessions.get_mut(&id) {
                Some(session) if session.title != title => {
                    session.title = title;
                    true
                }
                _ => false,
            },
            JobEvent::Deleted(id) => {
                let known = self.sessions.remove(&id).is_some();
                self.running.remove(&id) || known
            }
            JobEvent::ShellCreated(shell) => {
                if !self.accepts(&shell, context) || self.shells.get(&shell.id) == Some(&shell) {
                    return false;
                }
                self.shells.insert(shell.id.clone(), shell);
                true
            }
            JobEvent::ShellRemoved(id) => self.shells.remove(&id).is_some(),
        }
    }

    fn is_known_root(&self, id: &str) -> bool {
        self.sessions
            .get(id)
            .is_some_and(|session| session.parent_id.is_none())
    }

    /// A live shell is listed when a refresh would list it (its location is
    /// covered) or when it belongs to a session the client knows.
    fn accepts(&self, shell: &ShellJob, context: &Context) -> bool {
        context
            .directories
            .iter()
            .any(|directory| directory == &shell.directory)
            || self.locations.contains(&shell.directory)
            || shell
                .session_id
                .as_deref()
                .is_some_and(|id| context.is_root(id) || self.sessions.contains_key(id))
    }

    /// Applies a refresh, then replays the events seen while it ran.
    /// `active` is the server's running sessions, `None` when that list
    /// failed (running children are then kept). Shells of a location whose
    /// list failed are kept; those of locations no longer listed go.
    pub fn apply_snapshot(
        &mut self,
        active: Option<&HashSet<String>>,
        shells: ShellSnapshot,
        context: &Context,
    ) {
        if let Some(active) = active {
            self.running = active
                .iter()
                .filter(|id| !context.is_root(id) && !self.is_known_root(id))
                .cloned()
                .collect();
        }
        let ShellSnapshot {
            shells,
            queried,
            covered,
            ..
        } = shells;
        self.shells.retain(|_, shell| {
            queried.contains(&shell.directory) && !covered.contains(&shell.directory)
        });
        for shell in shells {
            self.shells.insert(shell.id.clone(), shell);
        }
        self.locations = covered;
        self.unresolved.clear();
        for event in self.journal.take().unwrap_or_default() {
            self.apply(event, context);
        }
        let reachable = self.reachable(context);
        self.sessions.retain(|id, _| reachable.contains(id));
    }

    /// Cached sessions on the way from a job to its root.
    fn reachable(&self, context: &Context) -> HashSet<String> {
        let mut reachable = HashSet::new();
        for start in self.owners() {
            let mut id = start;
            for _ in 0..MAX_PARENT_DEPTH {
                if context.is_root(id) {
                    break;
                }
                let Some(session) = self.sessions.get(id) else {
                    break;
                };
                reachable.insert(id.to_owned());
                let Some(parent) = session.parent_id.as_deref() else {
                    break;
                };
                id = parent;
            }
        }
        reachable
    }

    /// Running sessions and shell owners.
    fn owners(&self) -> impl Iterator<Item = &str> {
        self.running.iter().map(String::as_str).chain(
            self.shells
                .values()
                .filter_map(|shell| shell.session_id.as_deref()),
        )
    }

    /// Sessions whose info the list still needs: running sessions and shell
    /// owners outside the root list, and the parents above them up to a
    /// root. Each is returned once, until it loads or fails.
    pub fn take_wanted(&mut self, context: &Context) -> Vec<String> {
        let mut wanted = BTreeSet::new();
        for start in self.owners() {
            let mut id = start;
            for _ in 0..MAX_PARENT_DEPTH {
                if context.is_root(id) {
                    break;
                }
                match self.sessions.get(id) {
                    Some(session) => match session.parent_id.as_deref() {
                        Some(parent) => id = parent,
                        None => break,
                    },
                    None => {
                        if !self.requested.contains(id) && !self.unresolved.contains(id) {
                            wanted.insert(id.to_owned());
                        }
                        break;
                    }
                }
            }
        }
        self.requested.extend(wanted.iter().cloned());
        wanted.into_iter().collect()
    }

    /// Infos loaded for [`Jobs::take_wanted`]. A failure is not retried
    /// before the next refresh.
    pub fn apply_session_info(&mut self, results: Vec<(String, Result<Session, String>)>) {
        for (id, result) in results {
            self.requested.remove(&id);
            match result {
                Ok(session) => {
                    if session.parent_id.is_none() {
                        self.running.remove(&id);
                    }
                    self.sessions.insert(id, session);
                }
                Err(_) => {
                    self.unresolved.insert(id);
                }
            }
        }
    }

    /// The rows, oldest job first. A running session shows once its info is
    /// known and it has a parent.
    pub fn rows(&self, roots: &[Session]) -> Vec<JobRow> {
        let roots: HashMap<&str, &Session> = roots
            .iter()
            .filter(|session| session.parent_id.is_none())
            .map(|session| (session.id.as_str(), session))
            .collect();
        let title_of = |id: &str| {
            roots
                .get(id)
                .map(|session| session.title.clone())
                .or_else(|| self.sessions.get(id).map(|session| session.title.clone()))
        };
        let mut rows = Vec::new();
        for id in &self.running {
            let Some(session) = self.sessions.get(id) else {
                continue;
            };
            let Some(parent) = session.parent_id.as_deref() else {
                continue;
            };
            rows.push(JobRow {
                id: id.clone(),
                kind: JobKind::Subagent,
                title: session.title.clone(),
                owner: title_of(parent),
                root: self.root_of(parent, &roots),
                started: session.time.created,
            });
        }
        for shell in self.shells.values() {
            let owner = shell.session_id.as_deref();
            rows.push(JobRow {
                id: shell.id.clone(),
                kind: JobKind::Shell,
                title: command_title(&shell.command),
                owner: owner.and_then(title_of),
                root: owner.and_then(|id| self.root_of(id, &roots)),
                started: shell.started,
            });
        }
        rows.sort_by(|a, b| a.started.cmp(&b.started).then_with(|| a.id.cmp(&b.id)));
        rows
    }

    /// Walks `parentID` up from `id` to a listed root session.
    fn root_of(&self, id: &str, roots: &HashMap<&str, &Session>) -> Option<String> {
        let mut id = id;
        for _ in 0..MAX_PARENT_DEPTH {
            if roots.contains_key(id) {
                return Some(id.to_owned());
            }
            id = self.sessions.get(id)?.parent_id.as_deref()?;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;
    use crate::model::SessionTime;

    const NOW: u64 = 1_790_000_000_000;

    fn session(id: &str, parent: Option<&str>, title: &str, created: u64) -> Session {
        Session {
            id: id.into(),
            directory: "/repo".into(),
            title: title.into(),
            time: SessionTime {
                created,
                updated: created,
                archived: None,
            },
            parent_id: parent.map(str::to_owned),
            model: None,
        }
    }

    fn roots() -> Vec<Session> {
        vec![
            session("ses_a", None, "Fix the attach clip padding", 1),
            session("ses_b", None, "Reflection projection retries", 2),
        ]
    }

    fn shell(
        id: &str,
        directory: &str,
        owner: Option<&str>,
        command: &str,
        started: u64,
    ) -> ShellJob {
        ShellJob {
            id: id.into(),
            directory: directory.into(),
            command: command.into(),
            session_id: owner.map(str::to_owned),
            started,
        }
    }

    fn snapshot(shells: Vec<ShellJob>, queried: &[&str], covered: &[&str]) -> ShellSnapshot {
        ShellSnapshot {
            shells,
            queried: queried.iter().map(|d| d.to_string()).collect(),
            covered: covered.iter().map(|d| d.to_string()).collect(),
            warnings: Vec::new(),
        }
    }

    fn active(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn event(value: Value) -> Option<JobEvent> {
        let event: protocol::Event = serde_json::from_value(value).unwrap();
        let kind = protocol::decode_event(&event);
        job_event(&event, &kind, event.directory())
    }

    /// `(id, kind, title, owner, root)`.
    type RowSummary<'a> = (&'a str, JobKind, &'a str, Option<&'a str>, Option<&'a str>);

    fn summary(rows: &[JobRow]) -> Vec<RowSummary<'_>> {
        rows.iter()
            .map(|row| {
                (
                    row.id.as_str(),
                    row.kind,
                    row.title.as_str(),
                    row.owner.as_deref(),
                    row.root.as_deref(),
                )
            })
            .collect()
    }

    #[test]
    fn rows_combine_running_children_and_shells() {
        let roots = roots();
        let directories = vec!["/repo".to_owned()];
        let context = Context {
            roots: &roots,
            directories: &directories,
        };
        let mut jobs = Jobs::default();
        jobs.apply_snapshot(
            Some(&active(&["ses_a", "ses_child", "ses_grandchild"])),
            snapshot(
                vec![
                    shell(
                        "sh_dev",
                        "/repo",
                        Some("ses_a"),
                        "pnpm dev --port 5173\n",
                        NOW - 22 * 60_000,
                    ),
                    shell(
                        "sh_test",
                        "/repo",
                        Some("ses_grandchild"),
                        "  \ncargo test --all-targets",
                        NOW - 60_000,
                    ),
                    shell("sh_orphan", "/repo", None, "sleep 100", NOW - 1_000),
                ],
                &["/repo"],
                &["/repo"],
            ),
            &context,
        );
        assert!(
            jobs.rows(&roots)
                .iter()
                .all(|row| row.kind == JobKind::Shell),
            "children show once their info is known"
        );
        let wanted = jobs.take_wanted(&context);
        assert_eq!(
            wanted,
            ["ses_child", "ses_grandchild"],
            "the running root is not a job"
        );
        assert!(jobs.take_wanted(&context).is_empty(), "requested once");
        jobs.apply_session_info(vec![
            (
                "ses_grandchild".into(),
                Ok(session(
                    "ses_grandchild",
                    Some("ses_child"),
                    "Nested audit",
                    NOW - 2 * 60_000,
                )),
            ),
            (
                "ses_child".into(),
                Ok(session(
                    "ses_child",
                    Some("ses_a"),
                    "Audit v1 call sites",
                    NOW - 4 * 60_000,
                )),
            ),
        ]);
        assert!(jobs.take_wanted(&context).is_empty());
        let rows = jobs.rows(&roots);
        assert_eq!(
            summary(&rows),
            [
                (
                    "sh_dev",
                    JobKind::Shell,
                    "pnpm dev --port 5173",
                    Some("Fix the attach clip padding"),
                    Some("ses_a")
                ),
                (
                    "ses_child",
                    JobKind::Subagent,
                    "Audit v1 call sites",
                    Some("Fix the attach clip padding"),
                    Some("ses_a")
                ),
                (
                    "ses_grandchild",
                    JobKind::Subagent,
                    "Nested audit",
                    Some("Audit v1 call sites"),
                    Some("ses_a")
                ),
                (
                    "sh_test",
                    JobKind::Shell,
                    "cargo test --all-targets",
                    Some("Nested audit"),
                    Some("ses_a")
                ),
                ("sh_orphan", JobKind::Shell, "sleep 100", None, None),
            ]
        );
        assert_eq!(
            rows[0].subtitle(NOW),
            "shell · Fix the attach clip padding · 22m"
        );
        assert_eq!(rows[4].subtitle(NOW), "shell · <1m");
    }

    #[test]
    fn an_unknown_owner_is_fetched_once_and_left_unresolved_on_failure() {
        let roots = roots();
        let context = Context {
            roots: &roots,
            directories: &[],
        };
        let mut jobs = Jobs::default();
        jobs.apply_snapshot(
            None,
            snapshot(
                vec![shell("sh_1", "/repo", Some("ses_gone"), "make", 5)],
                &["/repo"],
                &["/repo"],
            ),
            &context,
        );
        assert_eq!(jobs.take_wanted(&context), ["ses_gone"]);
        jobs.apply_session_info(vec![("ses_gone".into(), Err("Session not found".into()))]);
        assert!(jobs.take_wanted(&context).is_empty(), "no retry storm");
        let rows = jobs.rows(&roots);
        assert_eq!(rows[0].owner, None);
        assert_eq!(rows[0].root, None, "not activatable");
        // The next refresh retries it.
        jobs.begin_refresh();
        jobs.apply_snapshot(
            None,
            snapshot(
                vec![shell("sh_1", "/repo", Some("ses_gone"), "make", 5)],
                &["/repo"],
                &["/repo"],
            ),
            &context,
        );
        assert_eq!(jobs.take_wanted(&context), ["ses_gone"]);
    }

    #[test]
    fn a_running_unlisted_root_is_not_a_job() {
        let roots = roots();
        let context = Context {
            roots: &roots,
            directories: &[],
        };
        let mut jobs = Jobs::default();
        assert!(jobs.apply_event(JobEvent::Started("ses_archived".into()), &context));
        assert_eq!(jobs.take_wanted(&context), ["ses_archived"]);
        jobs.apply_session_info(vec![(
            "ses_archived".into(),
            Ok(session("ses_archived", None, "Archived", 1)),
        )]);
        assert!(jobs.rows(&roots).is_empty());
        assert!(!jobs.apply_event(JobEvent::Started("ses_archived".into()), &context));
        assert!(
            !jobs.apply_event(JobEvent::Started("ses_a".into()), &context),
            "listed roots are chat turns"
        );
    }

    #[test]
    fn execution_events_add_and_remove_children() {
        let roots = roots();
        let context = Context {
            roots: &roots,
            directories: &[],
        };
        let mut jobs = Jobs::default();
        let created = event(json!({
            "id": "evt_1", "created": NOW - 1_000, "type": "session.created",
            "location": { "directory": "/repo" },
            "data": { "sessionID": "ses_child", "parentID": "ses_b", "title": "Mock child task",
                      "location": { "directory": "/repo" } }
        }))
        .unwrap();
        assert!(
            !jobs.apply_event(created, &context),
            "created, not running yet"
        );
        let started = event(json!({
            "id": "evt_2", "type": "session.execution.started", "data": { "sessionID": "ses_child" }
        }))
        .unwrap();
        assert!(jobs.apply_event(started, &context));
        assert!(
            jobs.take_wanted(&context).is_empty(),
            "session.created supplied the info"
        );
        let rows = jobs.rows(&roots);
        assert_eq!(
            summary(&rows),
            [(
                "ses_child",
                JobKind::Subagent,
                "Mock child task",
                Some("Reflection projection retries"),
                Some("ses_b")
            )]
        );
        assert_eq!(rows[0].started, NOW - 1_000);
        let renamed = event(json!({
            "id": "evt_3", "type": "session.renamed", "data": { "sessionID": "ses_child", "title": "Renamed task" }
        }))
        .unwrap();
        assert!(jobs.apply_event(renamed, &context));
        assert_eq!(jobs.rows(&roots)[0].title, "Renamed task");
        for (index, kind) in ["succeeded", "failed", "interrupted"]
            .into_iter()
            .enumerate()
        {
            let ended = event(json!({
                "id": format!("evt_end_{index}"),
                "type": format!("session.execution.{kind}"),
                "data": { "sessionID": "ses_child", "reason": "user", "error": { "type": "x", "message": "y" } }
            }))
            .unwrap();
            assert!(jobs.apply_event(ended, &context), "{kind}");
            assert!(jobs.rows(&roots).is_empty(), "{kind}");
            assert!(jobs.apply_event(JobEvent::Started("ses_child".into()), &context));
        }
    }

    #[test]
    fn shell_events_create_exit_delete_and_dedupe() {
        let roots = roots();
        let directories = vec!["/repo".to_owned()];
        let context = Context {
            roots: &roots,
            directories: &directories,
        };
        let mut jobs = Jobs::default();
        let created = |id: &str, directory: &str, owner: Value| {
            event(json!({
                "id": format!("evt_{id}"), "type": "shell.created",
                "location": { "directory": directory },
                "data": { "info": {
                    "id": id, "status": "running", "command": "sleep 5", "cwd": directory,
                    "shell": "/bin/bash", "file": "/tmp/out", "metadata": { "sessionID": owner },
                    "time": { "started": NOW - 5_000 }
                } }
            }))
            .unwrap()
        };
        assert!(jobs.apply_event(created("sh_1", "/repo", json!("ses_a")), &context));
        assert!(
            !jobs.apply_event(created("sh_1", "/repo", json!("ses_a")), &context),
            "dedupe by ID"
        );
        assert!(
            jobs.apply_event(created("sh_2", "/elsewhere", json!("ses_b")), &context),
            "owned by a known session"
        );
        assert!(
            !jobs.apply_event(
                created("sh_3", "/elsewhere", json!("ses_unknown")),
                &context
            ),
            "neither a listed location nor a known owner"
        );
        assert_eq!(jobs.rows(&roots).len(), 2);
        let exited = event(json!({
            "id": "evt_x", "type": "shell.exited", "location": { "directory": "/repo" },
            "data": { "id": "sh_1", "exit": 0, "status": "exited" }
        }))
        .unwrap();
        assert!(jobs.apply_event(exited, &context));
        let deleted = event(json!({
            "id": "evt_d", "type": "shell.deleted", "location": { "directory": "/elsewhere" },
            "data": { "id": "sh_2" }
        }))
        .unwrap();
        assert!(jobs.apply_event(deleted, &context));
        assert!(jobs.rows(&roots).is_empty(), "hidden when empty");
        assert!(!jobs.apply_event(JobEvent::ShellRemoved("sh_1".into()), &context));
        let exited_shell: protocol::ShellInfo = serde_json::from_value(json!({
            "id": "sh_4", "status": "exited", "command": "true", "metadata": {}
        }))
        .unwrap();
        assert_eq!(ShellJob::running(&exited_shell, "/repo"), None);
    }

    #[test]
    fn a_snapshot_keeps_failed_locations_and_drops_unlisted_ones() {
        let roots = roots();
        let directories = vec!["/a".to_owned(), "/b".to_owned(), "/c".to_owned()];
        let context = Context {
            roots: &roots,
            directories: &directories,
        };
        let mut jobs = Jobs::default();
        jobs.apply_snapshot(
            Some(&active(&[])),
            snapshot(
                vec![
                    shell("sh_a", "/a", Some("ses_a"), "a", 1),
                    shell("sh_b", "/b", Some("ses_a"), "b", 2),
                    shell("sh_c", "/c", Some("ses_a"), "c", 3),
                ],
                &["/a", "/b", "/c"],
                &["/a", "/b", "/c"],
            ),
            &context,
        );
        // /a lists nothing now, /b fails, /c is no longer listed.
        jobs.apply_snapshot(
            Some(&active(&[])),
            snapshot(
                vec![shell("sh_a2", "/a", Some("ses_a"), "a2", 4)],
                &["/a", "/b"],
                &["/a"],
            ),
            &context,
        );
        let ids: Vec<_> = jobs.rows(&roots).into_iter().map(|row| row.id).collect();
        assert_eq!(ids, ["sh_b", "sh_a2"]);
    }

    #[test]
    fn a_failed_active_list_keeps_running_children() {
        let roots = roots();
        let context = Context {
            roots: &roots,
            directories: &[],
        };
        let mut jobs = Jobs::default();
        jobs.apply_event(
            JobEvent::ChildCreated(session("ses_c", Some("ses_a"), "Task", 1)),
            &context,
        );
        jobs.apply_event(JobEvent::Started("ses_c".into()), &context);
        jobs.apply_snapshot(None, ShellSnapshot::default(), &context);
        assert_eq!(jobs.rows(&roots).len(), 1);
        jobs.apply_snapshot(Some(&active(&[])), ShellSnapshot::default(), &context);
        assert!(jobs.rows(&roots).is_empty());
    }

    #[test]
    fn events_during_a_refresh_are_replayed_onto_its_snapshot() {
        let roots = roots();
        let directories = vec!["/repo".to_owned()];
        let context = Context {
            roots: &roots,
            directories: &directories,
        };
        let mut jobs = Jobs::default();
        jobs.begin_refresh();
        // While the lists load: sh_1 exits and ses_c ends, then sh_2 starts.
        jobs.apply_event(JobEvent::ShellRemoved("sh_1".into()), &context);
        jobs.apply_event(JobEvent::Ended("ses_c".into()), &context);
        jobs.apply_event(
            JobEvent::ShellCreated(shell("sh_2", "/repo", Some("ses_a"), "two", 9)),
            &context,
        );
        jobs.apply_snapshot(
            Some(&active(&["ses_c"])),
            snapshot(
                vec![shell("sh_1", "/repo", Some("ses_a"), "one", 1)],
                &["/repo"],
                &["/repo"],
            ),
            &context,
        );
        jobs.apply_session_info(vec![(
            "ses_c".into(),
            Ok(session("ses_c", Some("ses_a"), "C", 1)),
        )]);
        let ids: Vec<_> = jobs.rows(&roots).into_iter().map(|row| row.id).collect();
        assert_eq!(ids, ["sh_2"]);
        // Only one refresh is replayed.
        jobs.apply_event(JobEvent::ShellRemoved("sh_2".into()), &context);
        jobs.apply_snapshot(
            None,
            snapshot(
                vec![shell("sh_2", "/repo", Some("ses_a"), "two", 9)],
                &["/repo"],
                &["/repo"],
            ),
            &context,
        );
        assert_eq!(jobs.rows(&roots).len(), 1);
        jobs.begin_refresh();
        jobs.apply_event(JobEvent::ShellRemoved("sh_2".into()), &context);
        jobs.abandon_refresh();
        jobs.apply_snapshot(
            None,
            snapshot(
                vec![shell("sh_3", "/repo", None, "three", 9)],
                &["/repo"],
                &["/repo"],
            ),
            &context,
        );
        let ids: Vec<_> = jobs.rows(&roots).into_iter().map(|row| row.id).collect();
        assert_eq!(ids, ["sh_3"]);
    }

    #[test]
    fn clearing_forgets_everything() {
        let roots = roots();
        let context = Context {
            roots: &roots,
            directories: &[],
        };
        let mut jobs = Jobs::default();
        jobs.apply_event(
            JobEvent::ShellCreated(shell("sh_1", "/x", Some("ses_a"), "x", 1)),
            &context,
        );
        jobs.begin_refresh();
        jobs.clear();
        assert!(jobs.rows(&roots).is_empty());
        jobs.apply_snapshot(None, ShellSnapshot::default(), &context);
        assert!(jobs.rows(&roots).is_empty(), "the journal went too");
    }

    #[test]
    fn elapsed_times_are_compact() {
        for (ms, expected) in [
            (0, "<1m"),
            (59_999, "<1m"),
            (4 * 60_000 + 59_000, "4m"),
            (60 * 60_000, "1h"),
            (65 * 60_000, "1h 5m"),
            (26 * 3_600_000, "1d 2h"),
            (48 * 3_600_000 + 60_000, "2d"),
        ] {
            assert_eq!(format_elapsed(ms), expected, "{ms}");
        }
        let row = JobRow {
            id: "x".into(),
            kind: JobKind::Subagent,
            title: "t".into(),
            owner: Some("Owner".into()),
            root: None,
            started: 0,
        };
        assert_eq!(
            row.subtitle(NOW),
            "subagent · Owner",
            "no elapsed time without a start"
        );
    }
}
