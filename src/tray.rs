//! Sending while a session runs (steer / queue) and the tray of waiting
//! prompts above the composer.
//!
//! Server behavior (2.0.8, verified on the harness): a steered prompt joins
//! the running turn at its next step; a queued one starts a new turn once
//! the turn ends. `POST /interrupt` without `resume` parks every waiting
//! item. On a parked session, switching an item to steer wakes the session,
//! which then delivers every parked steer at once and afterwards runs the
//! parked queue one turn at a time. `POST /interrupt?resume=true` wakes it for
//! the parked steers only; queued items stay parked.

use std::collections::HashSet;

use crate::{
    api::{InboxRequest, Settled},
    model::TrayItem,
    protocol::Delivery,
};

/// What a send does, from the composer's buttons or Enter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendMode {
    /// Idle session: a plain prompt (server default delivery).
    Send,
    /// Running session: into the current run at its next step.
    Steer,
    /// Running session: a new turn once the run ends.
    Queue,
}

impl SendMode {
    /// The prompt body's `delivery`: only a queued prompt carries one.
    pub fn delivery(self) -> Option<Delivery> {
        (self == Self::Queue).then_some(Delivery::Queue)
    }
}

/// Enter without Shift (Shift+Enter is a newline). Idle sessions send, with
/// or without Ctrl; while running, Enter steers and Ctrl+Enter queues.
pub fn enter_mode(busy: bool, ctrl: bool) -> SendMode {
    match (busy, ctrl) {
        (false, _) => SendMode::Send,
        (true, false) => SendMode::Steer,
        (true, true) => SendMode::Queue,
    }
}

/// The other mode, for a row's switch button.
pub fn switched(delivery: Delivery) -> Delivery {
    match delivery {
        Delivery::Queue => Delivery::Steer,
        _ => Delivery::Queue,
    }
}

/// "Send now" on a parked row. A queued item is switched to steer, which
/// wakes the session (the rest of the parked items then follow, steers first).
/// A parked steer already has that mode (a PATCH would be a 409 that wakes
/// nothing), so the session is resumed instead, which delivers the parked
/// steers and leaves queued items parked.
pub fn send_now_request(delivery: Delivery) -> InboxRequest {
    match delivery {
        Delivery::Queue => InboxRequest::SetDelivery(Delivery::Steer),
        _ => InboxRequest::Resume,
    }
}

/// A tray row's buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    Switch,
    SendNow,
    Cancel,
}

/// The request for a row action, or `None` while the row cannot act (its
/// prompt is still being sent, or a request for it is in flight).
pub fn row_request(row: &TrayRow, action: RowAction) -> Option<InboxRequest> {
    if row.sending || row.in_flight {
        return None;
    }
    Some(match action {
        RowAction::Switch => InboxRequest::SetDelivery(switched(row.delivery)),
        RowAction::SendNow => send_now_request(row.delivery),
        RowAction::Cancel => InboxRequest::Cancel,
    })
}

/// What a tray request's answer means for the client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Settlement {
    /// Accepted; the inbox events move the row.
    Done,
    /// The item was no longer waiting in that mode (409/404: delivered,
    /// cancelled or switched meanwhile): resolved, reload the inbox.
    Reconcile,
    /// Any other failure: show it and keep the row.
    Failed(String),
}

pub fn settlement(request: InboxRequest, result: Result<Settled, String>) -> Settlement {
    match result {
        Ok(Settled::Done) => Settlement::Done,
        Ok(Settled::AlreadyResolved) => Settlement::Reconcile,
        Err(error) => {
            let what = match request {
                InboxRequest::SetDelivery(_) => "switch",
                InboxRequest::Cancel => "cancel",
                InboxRequest::Resume => "send",
            };
            Settlement::Failed(format!("Could not {what} the waiting message: {error}"))
        }
    }
}

/// "2 waiting" while the session runs, "2 parked" once it is idle with
/// items left (after Stop, or when a queued item waits behind nothing).
pub fn header_text(count: usize, parked: bool) -> String {
    format!("{count} {}", if parked { "parked" } else { "waiting" })
}

pub fn badge_text(delivery: Delivery) -> &'static str {
    match delivery {
        Delivery::Queue => "⏸ QUEUE",
        _ => "↪ STEER",
    }
}

pub fn switch_label(delivery: Delivery) -> &'static str {
    match switched(delivery) {
        Delivery::Queue => "→ Queue",
        _ => "→ Steer",
    }
}

pub fn switch_tooltip(delivery: Delivery) -> &'static str {
    match switched(delivery) {
        Delivery::Queue => "Queue it instead: sent as a new turn once this run finishes",
        _ => "Steer it instead: the agent reads it at its next step",
    }
}

/// "Send now" tooltips, true to what the server does with the rest.
pub fn send_now_tooltip(delivery: Delivery) -> &'static str {
    match delivery {
        Delivery::Queue => {
            "Steer it, which restarts the session: parked steers go with it, then the queued ones"
        }
        _ => "Resume the session with the parked steer messages; queued ones stay parked",
    }
}

/// One line for a tray row: the first non-blank line of the text, plus
/// "· N attachments".
pub fn summary(text: &str, attachments: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let more = text.lines().filter(|line| !line.trim().is_empty()).count() > 1;
    let mut summary = if more {
        format!("{line} …")
    } else {
        line.to_owned()
    };
    if attachments > 0 {
        let files = format!(
            "{attachments} attachment{}",
            if attachments == 1 { "" } else { "s" }
        );
        summary = if summary.is_empty() {
            files
        } else {
            format!("{summary} · {files}")
        };
    }
    summary
}

/// A prompt sent while the session runs, shown in the tray before the
/// server echoes it (`session.inbox.enqueued`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingEntry {
    pub id: String,
    pub delivery: Delivery,
    pub summary: String,
    /// The POST has been answered; the echo is still on its way.
    pub accepted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayRow {
    pub id: String,
    pub delivery: Delivery,
    pub summary: String,
    /// Not in the server's inbox yet (its POST is in flight).
    pub sending: bool,
    /// A switch/cancel/send-now request for it is in flight.
    pub in_flight: bool,
}

/// The tray: the session's waiting prompts, oldest first, then a prompt
/// being sent into the run. `hidden` is a prompt still shown as the
/// optimistic transcript row (sent while idle).
pub fn tray_rows(
    items: &[TrayItem],
    hidden: Option<&str>,
    pending: Option<&PendingEntry>,
    in_flight: &HashSet<String>,
) -> Vec<TrayRow> {
    let mut rows: Vec<TrayRow> = items
        .iter()
        .filter(|item| Some(item.id.as_str()) != hidden)
        .map(|item| TrayRow {
            id: item.id.clone(),
            delivery: item.delivery,
            summary: summary(&item.text, item.attachments),
            sending: false,
            in_flight: in_flight.contains(&item.id),
        })
        .collect();
    if let Some(pending) = pending {
        if !items.iter().any(|item| item.id == pending.id) {
            rows.push(TrayRow {
                id: pending.id.clone(),
                delivery: pending.delivery,
                summary: pending.summary.clone(),
                sending: !pending.accepted,
                in_flight: true,
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, delivery: Delivery, text: &str, attachments: usize) -> TrayItem {
        TrayItem {
            id: id.into(),
            delivery,
            text: text.into(),
            attachments,
        }
    }

    #[test]
    fn enter_sends_when_idle_and_steers_or_queues_while_running() {
        assert_eq!(enter_mode(false, false), SendMode::Send);
        assert_eq!(
            enter_mode(false, true),
            SendMode::Send,
            "Ctrl+Enter = Enter"
        );
        assert_eq!(enter_mode(true, false), SendMode::Steer);
        assert_eq!(enter_mode(true, true), SendMode::Queue);
        assert_eq!(SendMode::Send.delivery(), None);
        assert_eq!(SendMode::Steer.delivery(), None, "steer omits delivery");
        assert_eq!(SendMode::Queue.delivery(), Some(Delivery::Queue));
    }

    #[test]
    fn send_now_steers_a_queued_item_and_resumes_for_a_parked_steer() {
        assert_eq!(
            send_now_request(Delivery::Queue),
            InboxRequest::SetDelivery(Delivery::Steer)
        );
        assert_eq!(send_now_request(Delivery::Steer), InboxRequest::Resume);
    }

    #[test]
    fn row_actions_map_to_inbox_requests_unless_busy() {
        let row = |delivery, sending, in_flight| TrayRow {
            id: "msg_1".into(),
            delivery,
            summary: String::new(),
            sending,
            in_flight,
        };
        let steer = row(Delivery::Steer, false, false);
        assert_eq!(
            row_request(&steer, RowAction::Switch),
            Some(InboxRequest::SetDelivery(Delivery::Queue))
        );
        assert_eq!(
            row_request(&row(Delivery::Queue, false, false), RowAction::Switch),
            Some(InboxRequest::SetDelivery(Delivery::Steer))
        );
        assert_eq!(
            row_request(&steer, RowAction::SendNow),
            Some(InboxRequest::Resume)
        );
        assert_eq!(
            row_request(&steer, RowAction::Cancel),
            Some(InboxRequest::Cancel)
        );
        assert_eq!(
            row_request(&row(Delivery::Steer, false, true), RowAction::Cancel),
            None
        );
        assert_eq!(
            row_request(&row(Delivery::Steer, true, true), RowAction::Switch),
            None
        );
    }

    #[test]
    fn conflicts_reconcile_and_other_errors_keep_the_row() {
        let steer = InboxRequest::SetDelivery(Delivery::Steer);
        assert_eq!(settlement(steer, Ok(Settled::Done)), Settlement::Done);
        assert_eq!(
            settlement(InboxRequest::Cancel, Ok(Settled::AlreadyResolved)),
            Settlement::Reconcile
        );
        assert_eq!(
            settlement(steer, Ok(Settled::AlreadyResolved)),
            Settlement::Reconcile
        );
        assert_eq!(
            settlement(InboxRequest::Resume, Err("server returned 500".into())),
            Settlement::Failed("Could not send the waiting message: server returned 500".into())
        );
    }

    #[test]
    fn labels_follow_the_mockup() {
        assert_eq!(header_text(2, false), "2 waiting");
        assert_eq!(header_text(1, true), "1 parked");
        assert_eq!(badge_text(Delivery::Steer), "↪ STEER");
        assert_eq!(badge_text(Delivery::Queue), "⏸ QUEUE");
        assert_eq!(switch_label(Delivery::Steer), "→ Queue");
        assert_eq!(switch_label(Delivery::Queue), "→ Steer");
    }

    #[test]
    fn summaries_are_one_line_with_an_attachment_count() {
        assert_eq!(summary("  hello  ", 0), "hello");
        assert_eq!(summary("\nfirst\n\nsecond", 0), "first …");
        assert_eq!(summary("look", 1), "look · 1 attachment");
        assert_eq!(summary("look", 3), "look · 3 attachments");
        assert_eq!(summary("", 2), "2 attachments");
    }

    #[test]
    fn rows_list_items_oldest_first_then_the_prompt_being_sent() {
        let items = [
            item("msg_s", Delivery::Steer, "and keep the cap", 0),
            item("msg_q", Delivery::Queue, "then the changelog", 1),
            item("msg_idle", Delivery::Steer, "sent while idle", 0),
        ];
        let in_flight = HashSet::from(["msg_q".to_owned()]);
        let pending = PendingEntry {
            id: "msg_new".into(),
            delivery: Delivery::Queue,
            summary: "later".into(),
            accepted: false,
        };
        let rows = tray_rows(&items, Some("msg_idle"), Some(&pending), &in_flight);
        let view: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.id.as_str(),
                    row.summary.as_str(),
                    row.sending,
                    row.in_flight,
                )
            })
            .collect();
        assert_eq!(
            view,
            [
                ("msg_s", "and keep the cap", false, false),
                ("msg_q", "then the changelog · 1 attachment", false, true),
                ("msg_new", "later", true, true),
            ]
        );

        // Once echoed, the server's item replaces the pending entry.
        let echoed = [item("msg_new", Delivery::Queue, "later", 0)];
        let rows = tray_rows(&echoed, None, Some(&pending), &HashSet::new());
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].sending && !rows[0].in_flight);

        // Accepted but not echoed yet: no longer "sending", still inert.
        let accepted = PendingEntry {
            accepted: true,
            ..pending
        };
        let rows = tray_rows(&[], None, Some(&accepted), &HashSet::new());
        assert!(!rows[0].sending && rows[0].in_flight);
    }
}
