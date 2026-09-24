use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::protocol;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Session {
    pub id: String,
    pub directory: String,
    pub title: String,
    pub time: SessionTime,
    #[serde(default, rename = "parentID")]
    pub parent_id: Option<String>,
    /// The model saved on the session by the server; `None` follows the
    /// server's default model.
    #[serde(default)]
    pub model: Option<SessionModel>,
}

impl Session {
    pub fn model_selection(&self) -> Option<ModelSelection> {
        self.model.as_ref().map(|model| ModelSelection {
            provider_id: model.provider_id.clone(),
            model_id: model.id.clone(),
            variant: model.variant.clone(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionModel {
    pub id: String,
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(default)]
    pub variant: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionTime {
    pub created: u64,
    pub updated: u64,
    #[serde(default)]
    pub archived: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Project {
    pub worktree: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// A model plus optional variant. `model_id` is the catalog ID
/// (`ModelInfo.id`). The field names are persisted in `state.json`, so keep
/// them stable.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct ModelSelection {
    pub provider_id: String,
    pub model_id: String,
    #[serde(default)]
    pub variant: Option<String>,
}

impl ModelSelection {
    pub fn from_ref(model: &protocol::ModelRef) -> Self {
        Self {
            provider_id: model.provider_id.clone(),
            model_id: model.id.clone(),
            variant: model.variant.clone(),
        }
    }

    pub fn to_ref(&self) -> protocol::ModelRef {
        protocol::ModelRef {
            id: self.model_id.clone(),
            provider_id: self.provider_id.clone(),
            variant: self.variant.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOption {
    pub provider_id: String,
    /// Catalog ID (`ModelInfo.id`), which is what `ModelRef.id` takes.
    pub model_id: String,
    pub label: String,
    pub variants: Vec<String>,
    pub supports_attachments: bool,
    pub context_limit: Option<u64>,
}

/// One location's model catalog. Only display data is kept: provider
/// `settings`, `headers` and `body` (which carry API keys) never reach it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelCatalog {
    pub models: Vec<ModelOption>,
    /// The server's default model (`GET /api/model/default`), else the first
    /// model. Display only: it is never sent to the server.
    pub preferred: Option<ModelSelection>,
}

/// Input modalities that the composer can attach.
const ATTACHMENT_INPUTS: [&str; 4] = ["image", "pdf", "audio", "video"];

impl ModelCatalog {
    /// Builds the catalog from `GET /api/model` and `GET /api/model/default`.
    /// Disabled and deprecated models are left out. The provider ID labels
    /// the provider: display names need `GET /api/provider`, whose entries
    /// also carry provider settings.
    pub fn from_models(
        models: &[protocol::ModelInfo],
        default: Option<&protocol::ModelInfo>,
    ) -> Self {
        let mut options: Vec<ModelOption> = models
            .iter()
            .filter(|model| {
                model.enabled && model.status != Some(protocol::ModelStatus::Deprecated)
            })
            .map(|model| {
                let name = if model.name.trim().is_empty() {
                    model.id.as_str()
                } else {
                    model.name.as_str()
                };
                let mut variants: Vec<String> = model
                    .variants
                    .iter()
                    .map(|variant| variant.id.clone())
                    .collect();
                variants.sort_by_key(|variant| variant_rank(variant));
                variants.dedup();
                ModelOption {
                    provider_id: model.provider_id.clone(),
                    model_id: model.id.clone(),
                    label: format!("{} / {name}", model.provider_id),
                    variants,
                    supports_attachments: ATTACHMENT_INPUTS
                        .iter()
                        .any(|input| model.accepts_input(input)),
                    context_limit: Some(model.limit.context).filter(|limit| *limit > 0),
                }
            })
            .collect();
        options.sort_by(|left, right| left.label.to_lowercase().cmp(&right.label.to_lowercase()));
        let preferred = default
            .map(|model| ModelSelection {
                provider_id: model.provider_id.clone(),
                model_id: model.id.clone(),
                variant: None,
            })
            .filter(|selection| contains_model(&options, selection))
            .or_else(|| {
                options.first().map(|model| ModelSelection {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                    variant: None,
                })
            });
        Self {
            models: options,
            preferred,
        }
    }

    pub fn find(&self, selection: &ModelSelection) -> Option<&ModelOption> {
        self.models.iter().find(|model| {
            model.provider_id == selection.provider_id && model.model_id == selection.model_id
        })
    }
}

fn contains_model(models: &[ModelOption], selection: &ModelSelection) -> bool {
    models.iter().any(|model| {
        model.provider_id == selection.provider_id && model.model_id == selection.model_id
    })
}

/// The model the composer shows for a session: an explicit pick still in
/// flight, then the model saved on the session, then the catalog's default
/// (the server default, else the first model). Only the first two are server
/// state; the fallback is display only and must never be sent (P5).
pub fn displayed_model(
    pending: Option<&ModelSelection>,
    session: Option<&Session>,
    catalog: &ModelCatalog,
) -> Option<ModelSelection> {
    pending
        .cloned()
        .or_else(|| session.and_then(Session::model_selection))
        .or_else(|| catalog.preferred.clone())
}

/// The switch an explicit pick requests, or `None` when it matches what is
/// already shown. Re-picking a displayed default therefore leaves the session
/// following the server default instead of pinning it.
pub fn model_switch_for_pick(
    displayed: Option<&ModelSelection>,
    picked: ModelSelection,
) -> Option<ModelSelection> {
    (displayed != Some(&picked)).then_some(picked)
}

fn variant_rank(value: &str) -> (usize, String) {
    let rank = match value.to_ascii_lowercase().as_str() {
        "minimal" => 0,
        "low" => 1,
        "medium" => 2,
        "high" => 3,
        "xhigh" | "max" => 4,
        _ => 5,
    };
    (rank, value.to_ascii_lowercase())
}

/// Why the model catalogs must be refetched (R5.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogInvalidation {
    /// The event's location; `None` means every loaded catalog.
    pub directory: Option<String>,
    /// `location.shutdown`: the location's instance is gone, so the session
    /// list is refreshed as well.
    pub shutdown: bool,
}

impl CatalogInvalidation {
    /// `payload` is the whole `/api/event` event.
    pub fn from_event(payload: &Value) -> Option<Self> {
        let event = protocol::Event::deserialize(payload).ok()?;
        let shutdown = match protocol::decode_event(&event) {
            protocol::EventKind::LocationShutdown => true,
            protocol::EventKind::CatalogChanged(
                protocol::CatalogChange::Model
                | protocol::CatalogChange::Provider
                | protocol::CatalogChange::ModelsDev
                | protocol::CatalogChange::Integration
                | protocol::CatalogChange::Credential
                | protocol::CatalogChange::Config,
            ) => false,
            _ => return None,
        };
        Some(Self {
            directory: event
                .directory()
                .filter(|directory| !directory.is_empty())
                .map(str::to_owned),
            shutdown,
        })
    }
}

fn json_u64(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| {
            value
                .as_f64()
                .and_then(|n| (n.is_finite() && n >= 0.0).then_some(n as u64))
        })
}

fn message_context_tokens(info: &Value) -> Option<u64> {
    let tokens = info.get("tokens")?;
    json_u64(tokens.get("total"))
        .or_else(|| {
            Some(
                json_u64(tokens.get("input")).unwrap_or(0)
                    + json_u64(tokens.get("output")).unwrap_or(0)
                    + json_u64(tokens.get("reasoning")).unwrap_or(0)
                    + json_u64(tokens.pointer("/cache/read")).unwrap_or(0)
                    + json_u64(tokens.pointer("/cache/write")).unwrap_or(0),
            )
        })
        .filter(|total| *total > 0)
}

pub fn format_context_usage(used: u64, limit: u64) -> String {
    format!("{} / {}", compact_tokens(used), compact_tokens(limit))
}

fn compact_tokens(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    if n < 100_000 {
        let tenths = (n + 50) / 100;
        if tenths % 10 == 0 {
            format!("{}k", tenths / 10)
        } else {
            format!("{}.{}k", tenths / 10, tenths % 10)
        }
    } else if n < 1_000_000 {
        format!("{}k", (n + 500) / 1000)
    } else if n < 10_000_000 {
        let tenths = (n + 50_000) / 100_000;
        if tenths % 10 == 0 {
            format!("{}m", tenths / 10)
        } else {
            format!("{}.{}m", tenths / 10, tenths % 10)
        }
    } else {
        format!("{}m", (n + 500_000) / 1_000_000)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "YOU",
            Self::Assistant => "AGENT",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SegmentKind {
    Text,
    Reasoning,
    Tool,
    File,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Segment {
    key: String,
    kind: SegmentKind,
    text: String,
    image_url: Option<String>,
    created: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessage {
    pub id: String,
    pub role: Role,
    pub created: u64,
    segments: Vec<Segment>,
    error: Option<String>,
    context_tokens: Option<u64>,
}

impl ChatMessage {
    fn placeholder(id: impl Into<String>, role: Role) -> Self {
        Self {
            id: id.into(),
            role,
            created: 0,
            segments: Vec::new(),
            error: None,
            context_tokens: None,
        }
    }

    #[cfg(test)]
    pub fn render(&self) -> String {
        let mut blocks = Vec::new();
        for segment in &self.segments {
            if segment.text.trim().is_empty() {
                continue;
            }
            match segment.kind {
                SegmentKind::Text => blocks.push(segment.text.clone()),
                SegmentKind::Reasoning => {
                    blocks.push(format!("Reasoning\n{}", segment.text.trim()))
                }
                SegmentKind::Tool | SegmentKind::File => blocks.push(segment.text.clone()),
            }
        }
        if let Some(error) = &self.error {
            blocks.push(format!("Error: {error}"));
        }
        blocks.join("\n\n")
    }

    fn transcript_rows(&self) -> Vec<String> {
        let mut rows = Vec::new();
        let mut blocks = Vec::new();
        let mut images = Vec::new();
        let role = self.role.label();
        for segment in &self.segments {
            if matches!(segment.kind, SegmentKind::Tool | SegmentKind::Reasoning) {
                push_transcript_row(
                    &mut rows,
                    role,
                    blocks.join("\n\n"),
                    images,
                    self.created,
                    "",
                );
                blocks = Vec::new();
                images = Vec::new();
                if !segment.text.trim().is_empty() {
                    let (body, kind) = match segment.kind {
                        SegmentKind::Reasoning => {
                            (format!("Reasoning\n{}", segment.text.trim()), "reasoning")
                        }
                        _ => (segment.text.clone(), "tool"),
                    };
                    push_transcript_row(
                        &mut rows,
                        role,
                        body,
                        Vec::new(),
                        if segment.created == 0 {
                            self.created
                        } else {
                            segment.created
                        },
                        kind,
                    );
                }
                continue;
            }
            if !segment.text.trim().is_empty() {
                blocks.push(segment.text.clone());
            }
            if let Some(url) = &segment.image_url {
                images.push(url.clone());
            }
        }
        if !blocks.is_empty() || !images.is_empty() {
            push_transcript_row(
                &mut rows,
                role,
                blocks.join("\n\n"),
                images,
                self.created,
                "",
            );
        }
        if let Some(error) = &self.error {
            if error != "Aborted" {
                push_transcript_row(
                    &mut rows,
                    role,
                    error.clone(),
                    Vec::new(),
                    self.created,
                    "error",
                );
            }
        }
        rows
    }

    fn upsert_segment(&mut self, mut segment: Segment) {
        if let Some(existing) = self
            .segments
            .iter_mut()
            .find(|existing| existing.key == segment.key)
        {
            if segment.created == 0 {
                segment.created = existing.created;
            }
            *existing = segment;
        } else {
            self.segments.push(segment);
        }
    }

    fn append_delta(&mut self, key: &str, kind: SegmentKind, delta: &str) {
        if let Some(existing) = self
            .segments
            .iter_mut()
            .find(|existing| existing.key == key)
        {
            existing.text.push_str(delta);
            return;
        }
        self.segments.push(Segment {
            key: key.to_owned(),
            kind,
            text: delta.to_owned(),
            image_url: None,
            created: 0,
        });
    }
}

#[derive(Clone, Debug, Default)]
pub struct Conversation {
    pub messages: Vec<ChatMessage>,
    pub next_cursor: Option<String>,
    pub loaded: bool,
    error_sequence: u64,
}

impl Conversation {
    /// `entries` is one history page in chronological order (oldest first).
    pub fn replace_from_api(
        &mut self,
        entries: &[protocol::SessionMessage],
        next_cursor: Option<String>,
    ) {
        self.messages = entries.iter().filter_map(message_from_entry).collect();
        self.next_cursor = next_cursor;
        self.loaded = true;
    }

    /// Prepends an older page (chronological order), skipping known entries.
    pub fn prepend_from_api(
        &mut self,
        entries: &[protocol::SessionMessage],
        next_cursor: Option<String>,
    ) {
        let existing: HashMap<_, _> = self
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| (message.id.as_str(), index))
            .collect();
        let mut earlier: Vec<_> = entries
            .iter()
            .filter_map(message_from_entry)
            .filter(|message| !existing.contains_key(message.id.as_str()))
            .collect();
        earlier.append(&mut self.messages);
        self.messages = earlier;
        self.next_cursor = next_cursor;
        self.loaded = true;
    }

    #[cfg(test)]
    pub fn rendered_rows(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter_map(|message| {
                let content = message.render();
                (!content.is_empty()).then(|| format!("{}\n{content}", message.role.label()))
            })
            .collect()
    }

    pub fn transcript_rows(&self) -> Vec<String> {
        self.messages
            .iter()
            .flat_map(ChatMessage::transcript_rows)
            .collect()
    }

    /// A prompt's `id` is its user message ID once the server delivers it.
    pub fn has_user_message(&self, id: &str) -> bool {
        self.messages
            .iter()
            .any(|message| message.role == Role::User && message.id == id)
    }

    pub fn context_tokens(&self) -> Option<u64> {
        self.messages.iter().rev().find_map(|message| {
            (message.role == Role::Assistant)
                .then_some(message.context_tokens)
                .flatten()
        })
    }

    pub fn apply_event(&mut self, payload: &Value) -> bool {
        let Some(event_type) = payload.get("type").and_then(Value::as_str) else {
            return false;
        };
        let data = event_data(payload);

        match event_type {
            "message.updated" => data
                .get("info")
                .map(|info| self.upsert_message_info(info))
                .unwrap_or(false),
            "message.part.updated" => data
                .get("part")
                .map(|part| self.upsert_part(part))
                .unwrap_or(false),
            "message.part.delta" => self.apply_part_delta(data),
            "message.part.removed" => self.remove_part(data),
            "message.removed" => data
                .get("messageID")
                .and_then(Value::as_str)
                .map(|id| self.remove_message(id))
                .unwrap_or(false),
            "session.input.admitted" | "session.next.prompted" | "session.next.prompt.admitted" => {
                self.apply_input_admitted(data)
            }
            "session.step.started" | "session.next.step.started" => self.apply_step_started(data),
            "session.text.started" | "session.next.text.started" => {
                self.apply_stream_start(data, SegmentKind::Text)
            }
            "session.text.delta" | "session.next.text.delta" => {
                self.apply_stream_delta(data, SegmentKind::Text)
            }
            "session.text.ended" | "session.next.text.ended" => {
                self.apply_stream_end(data, SegmentKind::Text)
            }
            "session.reasoning.started" | "session.next.reasoning.started" => {
                self.apply_stream_start(data, SegmentKind::Reasoning)
            }
            "session.reasoning.delta" | "session.next.reasoning.delta" => {
                self.apply_stream_delta(data, SegmentKind::Reasoning)
            }
            "session.reasoning.ended" | "session.next.reasoning.ended" => {
                self.apply_stream_end(data, SegmentKind::Reasoning)
            }
            "session.tool.input.started"
            | "session.tool.called"
            | "session.tool.progress"
            | "session.next.tool.input.started"
            | "session.next.tool.called"
            | "session.next.tool.progress" => self.apply_tool_event(data, "running"),
            "session.tool.success" | "session.next.tool.success" => {
                self.apply_tool_event(data, "done")
            }
            "session.tool.failed" | "session.next.tool.failed" => {
                self.apply_tool_event(data, "failed")
            }
            "session.step.failed" | "session.next.step.failed" | "session.error" => {
                self.apply_error(data)
            }
            _ => false,
        }
    }

    fn upsert_message_info(&mut self, info: &Value) -> bool {
        let Some(id) = info.get("id").and_then(Value::as_str) else {
            return false;
        };
        let role = match info.get("role").and_then(Value::as_str) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => return false,
        };
        let created = info
            .pointer("/time/created")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let error = error_text(info.get("error"));
        let context_tokens = message_context_tokens(info);
        if let Some(message) = self.messages.iter_mut().find(|message| message.id == id) {
            message.role = role;
            message.created = created;
            message.error = error;
            if context_tokens.is_some() {
                message.context_tokens = context_tokens;
            }
            return true;
        }
        self.messages.push(ChatMessage {
            id: id.to_owned(),
            role,
            created,
            segments: Vec::new(),
            error,
            context_tokens,
        });
        true
    }

    fn upsert_part(&mut self, part: &Value) -> bool {
        let Some(message_id) = part.get("messageID").and_then(Value::as_str) else {
            return false;
        };
        let role = if part.get("type").and_then(Value::as_str) == Some("file") {
            Role::User
        } else {
            Role::Assistant
        };
        let index = self.ensure_message(message_id, role);
        let Some(segment) = segment_from_part(part) else {
            return false;
        };
        self.messages[index].upsert_segment(segment);
        true
    }

    fn apply_part_delta(&mut self, data: &Value) -> bool {
        if data.get("field").and_then(Value::as_str) != Some("text") {
            return false;
        }
        let Some(message_id) = data.get("messageID").and_then(Value::as_str) else {
            return false;
        };
        let Some(part_id) = data.get("partID").and_then(Value::as_str) else {
            return false;
        };
        let Some(delta) = data.get("delta").and_then(Value::as_str) else {
            return false;
        };
        let index = self.ensure_message(message_id, Role::Assistant);
        self.messages[index].append_delta(part_id, SegmentKind::Text, delta);
        true
    }

    fn remove_part(&mut self, data: &Value) -> bool {
        let Some(message_id) = data.get("messageID").and_then(Value::as_str) else {
            return false;
        };
        let Some(part_id) = data.get("partID").and_then(Value::as_str) else {
            return false;
        };
        let Some(message) = self
            .messages
            .iter_mut()
            .find(|message| message.id == message_id)
        else {
            return false;
        };
        let before = message.segments.len();
        message.segments.retain(|segment| segment.key != part_id);
        message.segments.len() != before
    }

    fn remove_message(&mut self, id: &str) -> bool {
        let before = self.messages.len();
        self.messages.retain(|message| message.id != id);
        self.messages.len() != before
    }

    fn apply_input_admitted(&mut self, data: &Value) -> bool {
        let Some(id) = data
            .get("inputID")
            .or_else(|| data.get("messageID"))
            .and_then(Value::as_str)
        else {
            return false;
        };
        let Some(text) = data
            .pointer("/input/data/text")
            .or_else(|| data.pointer("/prompt/text"))
            .and_then(Value::as_str)
        else {
            return false;
        };
        let index = self.ensure_message(id, Role::User);
        self.messages[index].upsert_segment(Segment {
            key: "text:0".to_owned(),
            kind: SegmentKind::Text,
            text: text.to_owned(),
            image_url: None,
            created: 0,
        });
        true
    }

    fn apply_step_started(&mut self, data: &Value) -> bool {
        let Some(id) = data.get("assistantMessageID").and_then(Value::as_str) else {
            return false;
        };
        self.ensure_message(id, Role::Assistant);
        true
    }

    fn apply_stream_start(&mut self, data: &Value, kind: SegmentKind) -> bool {
        let Some(message_id) = data.get("assistantMessageID").and_then(Value::as_str) else {
            return false;
        };
        let key = stream_key(data, &kind);
        let index = self.ensure_message(message_id, Role::Assistant);
        if self.messages[index]
            .segments
            .iter()
            .any(|segment| segment.key == key)
        {
            return false;
        }
        self.messages[index].upsert_segment(Segment {
            key,
            kind,
            text: String::new(),
            image_url: None,
            created: 0,
        });
        true
    }

    fn apply_stream_delta(&mut self, data: &Value, kind: SegmentKind) -> bool {
        let Some(message_id) = data.get("assistantMessageID").and_then(Value::as_str) else {
            return false;
        };
        let Some(delta) = data.get("delta").and_then(Value::as_str) else {
            return false;
        };
        let key = stream_key(data, &kind);
        let index = self.ensure_message(message_id, Role::Assistant);
        self.messages[index].append_delta(&key, kind, delta);
        true
    }

    fn apply_stream_end(&mut self, data: &Value, kind: SegmentKind) -> bool {
        let Some(message_id) = data.get("assistantMessageID").and_then(Value::as_str) else {
            return false;
        };
        let Some(text) = data.get("text").and_then(Value::as_str) else {
            return false;
        };
        let key = stream_key(data, &kind);
        let index = self.ensure_message(message_id, Role::Assistant);
        self.messages[index].upsert_segment(Segment {
            key,
            kind,
            text: text.to_owned(),
            image_url: None,
            created: 0,
        });
        true
    }

    fn apply_tool_event(&mut self, data: &Value, status: &str) -> bool {
        let Some(message_id) = data.get("assistantMessageID").and_then(Value::as_str) else {
            return false;
        };
        let call_id = data
            .get("callID")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let index = self.ensure_message(message_id, Role::Assistant);
        let key = format!("tool:{call_id}");
        let previous_title = self.messages[index]
            .segments
            .iter()
            .find(|segment| segment.key == key)
            .and_then(|segment| {
                segment
                    .text
                    .split_once(" · ")
                    .and_then(|(_, rest)| rest.split_once(" — "))
                    .map(|(_, title)| title.to_owned())
            });
        let previous_name = self.messages[index]
            .segments
            .iter()
            .find(|segment| segment.key == key)
            .and_then(|segment| {
                segment
                    .text
                    .split_once(" · ")
                    .map(|(name, _)| name.to_owned())
            });
        let name = data
            .get("name")
            .or_else(|| data.get("tool"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(previous_name)
            .unwrap_or_else(|| "tool".to_owned());
        let title = legacy_tool_title(data)
            .or(previous_title)
            .filter(|title| !title.is_empty())
            .map(|title| format!(" — {title}"))
            .unwrap_or_default();
        let detail = data
            .get("error")
            .and_then(|error| error_text(Some(error)))
            .map(|error| format!(": {error}"))
            .unwrap_or_default();
        self.messages[index].upsert_segment(Segment {
            key,
            kind: SegmentKind::Tool,
            text: format!("{name} · {status}{title}{detail}"),
            image_url: None,
            created: value_time(data),
        });
        true
    }

    fn apply_error(&mut self, data: &Value) -> bool {
        let message_id = data
            .get("assistantMessageID")
            .or_else(|| data.get("messageID"))
            .and_then(Value::as_str);
        let Some(error) = error_text(data.get("error")) else {
            return false;
        };
        if error == "Aborted"
            || data.pointer("/error/name").and_then(Value::as_str) == Some("MessageAbortedError")
        {
            return false;
        }
        let index = if let Some(message_id) = message_id {
            self.ensure_message(message_id, Role::Assistant)
        } else {
            let session_id = data
                .get("sessionID")
                .and_then(Value::as_str)
                .unwrap_or("session");
            let id = format!("{session_id}:error:{}", self.error_sequence);
            self.error_sequence += 1;
            let idx = self.ensure_message(&id, Role::Assistant);
            self.messages[idx].created = value_time(data);
            idx
        };
        self.messages[index].error = Some(error);
        true
    }

    fn ensure_message(&mut self, id: &str, role: Role) -> usize {
        if let Some(index) = self.messages.iter().position(|message| message.id == id) {
            return index;
        }
        self.messages.push(ChatMessage::placeholder(id, role));
        self.messages.len() - 1
    }
}

/// Projects one v2 message-list entry into the transcript model, or `None`
/// when the entry renders nothing (`idle`, unknown entries without text).
/// Every entry other than `user` renders as an AGENT message styled like
/// assistant text (R3.4).
///
/// Message and segment keys are shared with live events (R3.5), so a refresh
/// or a replay of events onto fresh history never duplicates rows:
/// - message: the entry `id`. Event-projected entries (switches, synthetic,
///   system, skill, shell, idle) carry the event ID with `evt_` replaced by
///   `msg_` ([`protocol::Event::projected_message_id`]); compaction uses its
///   `inputID` when present; user entries use the prompt/inbox `id`; assistant
///   entries use the `assistantMessageID` of their `session.step.*` events.
/// - assistant text and reasoning: `text:{k}` / `reasoning:{k}`, where `k` is
///   the item's index among same-kind items of `content[]`. That is the live
///   `ordinal` of `session.{text,reasoning}.{started,delta,ended}`.
/// - assistant tool: `tool:{id}`, the provider call ID that `session.tool.*`
///   events carry as `data.id`; it is unique only within one message.
/// - user text `text:0`, user files `file:{index}`; the other entries have a
///   single `text:0` segment.
pub fn message_from_entry(entry: &protocol::SessionMessage) -> Option<ChatMessage> {
    use protocol::SessionMessage as Entry;
    match entry {
        Entry::User(message) => Some(user_message(message)),
        Entry::Assistant(message) => Some(assistant_message(message)),
        Entry::Synthetic(note) => note_message(
            &note.id,
            note.time.created,
            non_blank(&note.text).or_else(|| note.description.as_deref().and_then(non_blank)),
        ),
        // System text is the full instruction set; its description names what changed.
        Entry::System(note) => note_message(
            &note.id,
            note.time.created,
            note.description
                .as_deref()
                .and_then(non_blank)
                .or_else(|| non_blank(&note.text)),
        ),
        // Skill text is the whole skill body, so only the name is shown.
        Entry::Skill(skill) => {
            let name = non_blank(&skill.name).or_else(|| non_blank(&skill.skill));
            note_message(
                &skill.id,
                skill.time.created,
                name.map(|name| format!("Skill: {name}")),
            )
        }
        Entry::Shell(shell) => note_message(&shell.id, shell.time.created, shell_body(shell)),
        Entry::Compaction(compaction) => note_message(
            &compaction.id,
            compaction.time.created,
            compaction_body(compaction),
        ),
        Entry::AgentSwitched(switch) => note_message(
            &switch.id,
            switch.time.created,
            non_blank(&switch.agent).map(|agent| format!("Switched agent to {agent}")),
        ),
        Entry::ModelSwitched(switch) => {
            let model = &switch.model;
            let variant = model
                .variant
                .as_deref()
                .and_then(non_blank)
                .map(|variant| format!(" ({variant})"))
                .unwrap_or_default();
            note_message(
                &switch.id,
                switch.time.created,
                Some(format!(
                    "Switched model to {}/{}{variant}",
                    model.provider_id, model.id
                )),
            )
        }
        Entry::LocationSwitched(switch) => note_message(
            &switch.id,
            switch.time.created,
            non_blank(&switch.location.directory).map(|directory| format!("Moved to {directory}")),
        ),
        Entry::Idle(_) => None,
        Entry::Unknown(unknown) => {
            let id = unknown.id.as_deref()?;
            let created = unknown
                .raw
                .pointer("/time/created")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let text = unknown
                .raw
                .get("text")
                .and_then(Value::as_str)
                .and_then(non_blank);
            note_message(id, created, text)
        }
    }
}

fn user_message(message: &protocol::UserMessage) -> ChatMessage {
    let mut segments = Vec::new();
    if !message.text.is_empty() {
        segments.push(Segment {
            key: "text:0".to_owned(),
            kind: SegmentKind::Text,
            text: message.text.clone(),
            image_url: None,
            created: 0,
        });
    }
    for (index, file) in message.files.iter().enumerate() {
        let name = file
            .name
            .as_deref()
            .and_then(non_blank)
            .unwrap_or("attachment");
        let mime = non_blank(&file.mime).unwrap_or("application/octet-stream");
        segments.push(Segment {
            key: format!("file:{index}"),
            kind: SegmentKind::File,
            text: format!("Attached: {name} ({mime})"),
            image_url: (mime.starts_with("image/") && !file.data.is_empty())
                .then(|| file.data_url()),
            created: 0,
        });
    }
    ChatMessage {
        id: message.id.clone(),
        role: Role::User,
        created: millis(message.time.created),
        segments,
        error: None,
        context_tokens: None,
    }
}

fn assistant_message(message: &protocol::AssistantMessage) -> ChatMessage {
    use protocol::AssistantContent as Content;
    let mut segments = Vec::new();
    let mut texts = 0;
    let mut reasonings = 0;
    for item in &message.content {
        match item {
            Content::Text(text) => {
                segments.push(Segment {
                    key: format!("text:{texts}"),
                    kind: SegmentKind::Text,
                    text: text.text.clone(),
                    image_url: None,
                    created: 0,
                });
                texts += 1;
            }
            Content::Reasoning(reasoning) => {
                segments.push(Segment {
                    key: format!("reasoning:{reasonings}"),
                    kind: SegmentKind::Reasoning,
                    text: reasoning.text.clone(),
                    image_url: None,
                    created: reasoning.time.map(|time| millis(time.created)).unwrap_or(0),
                });
                reasonings += 1;
            }
            Content::Tool(tool) => segments.push(tool_segment(tool)),
            // Keep ordinals aligned when a known kind fails to decode.
            Content::Unknown(unknown) => match unknown.kind.as_str() {
                "text" => texts += 1,
                "reasoning" => reasonings += 1,
                _ => {}
            },
        }
    }
    ChatMessage {
        id: message.id.clone(),
        role: Role::Assistant,
        created: millis(message.time.created),
        segments,
        error: message
            .error
            .as_ref()
            .filter(|error| !is_interrupt(error))
            .map(structured_error_text),
        context_tokens: message.tokens.as_ref().and_then(usage_tokens),
    }
}

/// An interrupted step fails with `type: "aborted"`; like v1 "Aborted" it is
/// not an error worth showing.
fn is_interrupt(error: &protocol::StructuredError) -> bool {
    error.kind == "aborted"
}

fn structured_error_text(error: &protocol::StructuredError) -> String {
    non_blank(&error.message)
        .or_else(|| non_blank(&error.kind))
        .unwrap_or("Request failed")
        .to_owned()
}

fn usage_tokens(tokens: &protocol::TokenUsage) -> Option<u64> {
    let total = tokens.total();
    (total.is_finite() && total >= 1.0).then(|| total.round() as u64)
}

/// `"{name} · {status}{ — title}{: error}"`, the tool row body.
fn tool_segment(tool: &protocol::ToolCall) -> Segment {
    use protocol::ToolState;
    let empty = protocol::JsonMap::new();
    let streamed: protocol::JsonMap;
    let (status, input, error) = match &tool.state {
        ToolState::Streaming { input } => {
            streamed = serde_json::from_str(input).unwrap_or_default();
            ("running", &streamed, None)
        }
        ToolState::Running { input, .. } => ("running", input, None),
        ToolState::Completed { input, .. } => ("completed", input, None),
        ToolState::Error { input, error, .. } => {
            ("error", input, Some(structured_error_text(error)))
        }
        ToolState::Unknown => ("pending", &empty, None),
    };
    let name = non_blank(&tool.name).unwrap_or("tool");
    let title = tool_title(name, input)
        .map(|title| format!(" — {title}"))
        .unwrap_or_default();
    let detail = error.map(|error| format!(": {error}")).unwrap_or_default();
    Segment {
        key: format!("tool:{}", tool.id),
        kind: SegmentKind::Tool,
        text: format!("{name} · {status}{title}{detail}"),
        image_url: None,
        created: millis(tool.time.created),
    }
}

/// A one-line summary of a tool call from its input, per v2 tool schema
/// (`core/src/tool/plugin/*.ts`); other tools (MCP, plugins) try common keys.
fn tool_title(name: &str, input: &protocol::JsonMap) -> Option<String> {
    let keys: &[&str] = match name {
        "shell" => &["command", "description"],
        "subagent" => &["description", "agent"],
        "read" | "write" | "edit" => &["path"],
        "glob" | "grep" => &["pattern"],
        "webfetch" => &["url"],
        "websearch" => &["query"],
        "skill" => &["id"],
        _ => &[
            "command",
            "description",
            "path",
            "filePath",
            "pattern",
            "query",
            "url",
        ],
    };
    keys.iter().find_map(|key| {
        input
            .get(*key)
            .and_then(Value::as_str)
            .and_then(single_line)
    })
}

/// v1 live tool events: an explicit title, else one derived from the input.
fn legacy_tool_title(value: &Value) -> Option<String> {
    value
        .pointer("/state/title")
        .or_else(|| value.get("title"))
        .and_then(Value::as_str)
        .and_then(single_line)
        .or_else(|| {
            let input = value
                .pointer("/state/input")
                .or_else(|| value.get("input"))?
                .as_object()?;
            let tool = value
                .get("tool")
                .or_else(|| value.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            tool_title(tool, input)
        })
}

fn single_line(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (!line.is_empty()).then_some(line)
}

fn non_blank(text: &str) -> Option<&str> {
    (!text.trim().is_empty()).then_some(text)
}

/// A non-user entry shown as one AGENT text segment.
fn note_message(
    id: &str,
    created: protocol::Millis,
    body: Option<impl Into<String>>,
) -> Option<ChatMessage> {
    let text = body?.into();
    Some(ChatMessage {
        id: id.to_owned(),
        role: Role::Assistant,
        created: millis(created),
        segments: vec![Segment {
            key: "text:0".to_owned(),
            kind: SegmentKind::Text,
            text,
            image_url: None,
            created: 0,
        }],
        error: None,
        context_tokens: None,
    })
}

/// `$ command` and its output as a code block, plus how it ended if abnormal.
fn shell_body(shell: &protocol::ShellMessage) -> Option<String> {
    let command = shell.command.trim();
    let output = shell
        .output
        .as_ref()
        .map(|output| output.output.trim_end())
        .filter(|output| !output.is_empty());
    if command.is_empty() && output.is_none() {
        return None;
    }
    let mut text = format!("$ {command}");
    if let Some(output) = output {
        text.push('\n');
        text.push_str(output);
    }
    let mut body = code_block(&text);
    match shell.status {
        protocol::ShellStatus::Timeout => body.push_str("\n\nTimed out"),
        protocol::ShellStatus::Killed => body.push_str("\n\nKilled"),
        _ => {}
    }
    Some(body)
}

/// A fenced block whose fence is longer than any backtick run inside.
fn code_block(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}\n{text}\n{fence}")
}

fn compaction_body(compaction: &protocol::CompactionMessage) -> Option<String> {
    use protocol::CompactionStatus;
    let summary = non_blank(&compaction.summary).map(str::to_owned);
    match compaction.status {
        CompactionStatus::Running => Some("Compacting the conversation…".to_owned()),
        CompactionStatus::Completed => {
            summary.or_else(|| Some("Compacted the conversation.".to_owned()))
        }
        CompactionStatus::Failed => Some(
            match compaction
                .error
                .as_ref()
                .and_then(|error| non_blank(&error.message).or_else(|| non_blank(&error.kind)))
            {
                Some(error) => format!("Compaction failed: {error}"),
                None => "Compaction failed.".to_owned(),
            },
        ),
        CompactionStatus::Unknown => summary,
    }
}

fn segment_from_part(part: &Value) -> Option<Segment> {
    if part
        .get("ignored")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || part
            .get("synthetic")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return None;
    }
    let part_type = part.get("type")?.as_str()?;
    let key = part
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{part_type}:unknown"));
    let created = value_time(part);
    match part_type {
        "text" => Some(Segment {
            key,
            kind: SegmentKind::Text,
            text: part.get("text")?.as_str()?.to_owned(),
            image_url: None,
            created,
        }),
        "reasoning" => Some(Segment {
            key,
            kind: SegmentKind::Reasoning,
            text: part.get("text")?.as_str()?.to_owned(),
            image_url: None,
            created,
        }),
        "file" => {
            let filename = part
                .get("filename")
                .and_then(Value::as_str)
                .unwrap_or("attachment");
            let mime = part
                .get("mime")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream");
            Some(Segment {
                key,
                kind: SegmentKind::File,
                text: format!("Attached: {filename} ({mime})"),
                image_url: mime
                    .starts_with("image/")
                    .then(|| part.get("url").and_then(Value::as_str).map(str::to_owned))
                    .flatten(),
                created,
            })
        }
        "tool" => {
            let name = part.get("tool").and_then(Value::as_str).unwrap_or("tool");
            let status = part
                .pointer("/state/status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            let title = legacy_tool_title(part)
                .filter(|title| !title.is_empty())
                .map(|title| format!(" — {title}"))
                .unwrap_or_default();
            let error = part
                .pointer("/state/error")
                .and_then(value_text)
                .map(|error| format!(": {error}"))
                .unwrap_or_default();
            Some(Segment {
                key,
                kind: SegmentKind::Tool,
                text: format!("{name} · {status}{title}{error}"),
                image_url: None,
                created,
            })
        }
        _ => None,
    }
}

fn stream_key(data: &Value, kind: &SegmentKind) -> String {
    let explicit = match kind {
        SegmentKind::Text => data.get("textID"),
        SegmentKind::Reasoning => data.get("reasoningID"),
        SegmentKind::Tool | SegmentKind::File => None,
    };
    if let Some(id) = explicit.and_then(Value::as_str) {
        return id.to_owned();
    }
    let prefix = match kind {
        SegmentKind::Text => "text",
        SegmentKind::Reasoning => "reasoning",
        SegmentKind::Tool => "tool",
        SegmentKind::File => "file",
    };
    let ordinal = data
        .get("ordinal")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    format!("{prefix}:{ordinal}")
}

fn error_text(error: Option<&Value>) -> Option<String> {
    let error = error?;
    value_text(error)
        .or_else(|| error.pointer("/data/message").and_then(value_text))
        .or_else(|| error.pointer("/error/message").and_then(value_text))
        .or_else(|| error.pointer("/data/error").and_then(value_text))
        .or_else(|| error.get("message").and_then(value_text))
        .or_else(|| error.get("error").and_then(value_text))
        .or_else(|| (!error.is_null()).then(|| error.to_string()))
}

fn value_text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

fn value_time(value: &Value) -> u64 {
    value
        .pointer("/time/start")
        .or_else(|| value.pointer("/time/created"))
        .or_else(|| value.pointer("/state/time/start"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn push_transcript_row(
    rows: &mut Vec<String>,
    role: &str,
    body: String,
    images: Vec<String>,
    time: u64,
    kind: &str,
) {
    if body.is_empty() && images.is_empty() {
        return;
    }
    rows.push(
        json!({
            "role": role,
            "body": body,
            "images": images,
            "time": time,
            "kind": kind,
        })
        .to_string(),
    );
}

pub fn event_data(payload: &Value) -> &Value {
    payload
        .get("properties")
        .or_else(|| payload.get("data"))
        .unwrap_or(&Value::Null)
}

pub fn event_session_id(payload: &Value) -> Option<&str> {
    let data = event_data(payload);
    data.get("sessionID")
        .or_else(|| data.pointer("/info/sessionID"))
        .or_else(|| data.pointer("/part/sessionID"))
        .and_then(Value::as_str)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Idle,
    Busy,
    Retry { message: String, attempt: u32 },
}

impl RunStatus {
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Busy | Self::Retry { .. })
    }

    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        match value.get("type").and_then(Value::as_str) {
            Some("idle") => Some(Self::Idle),
            Some("busy") => Some(Self::Busy),
            Some("retry") => {
                let message = value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Retrying...")
                    .to_owned();
                let attempt = value.get("attempt").and_then(Value::as_u64).unwrap_or(1) as u32;
                Some(Self::Retry { message, attempt })
            }
            _ => None,
        }
    }
}

pub fn event_run_status(payload: &Value) -> Option<(String, RunStatus)> {
    let event_type = payload.get("type")?.as_str()?;
    let data = event_data(payload);
    let session_id = event_session_id(payload)?.to_owned();
    match event_type {
        "session.status" => {
            let status = RunStatus::from_value(data.get("status")?)?;
            Some((session_id, status))
        }
        "session.idle"
        | "session.error"
        | "session.execution.succeeded"
        | "session.execution.failed"
        | "session.execution.interrupted" => Some((session_id, RunStatus::Idle)),
        "session.step.started" | "session.next.step.started" => Some((session_id, RunStatus::Busy)),
        _ => None,
    }
}

const UNTITLED_SESSION: &str = "Untitled session";

fn millis(value: protocol::Millis) -> u64 {
    value.max(0) as u64
}

fn display_title(title: Option<&str>) -> String {
    title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(UNTITLED_SESSION)
        .to_owned()
}

impl Session {
    /// Never copies `agent`: the client always uses the server's default agent.
    pub fn from_info(info: &protocol::SessionInfo) -> Self {
        Self {
            id: info.id.clone(),
            directory: info.directory().to_owned(),
            title: display_title(info.title.as_deref()),
            time: SessionTime {
                created: millis(info.time.created),
                updated: millis(info.time.updated),
                archived: info.time.archived.map(|archived| archived as f64),
            },
            parent_id: info.parent_id.clone(),
            model: info.model.as_ref().map(SessionModel::from_ref),
        }
    }

    /// `session.created` carries creation fields only; its time is the event's `created`.
    fn from_created(data: &protocol::SessionCreated, created: u64) -> Self {
        Self {
            id: data.session_id.clone(),
            directory: data.location.directory.clone(),
            title: display_title(data.title.as_deref()),
            time: SessionTime {
                created,
                updated: created,
                archived: None,
            },
            parent_id: data.parent_id.clone(),
            model: data.model.as_ref().map(SessionModel::from_ref),
        }
    }
}

impl SessionModel {
    fn from_ref(model: &protocol::ModelRef) -> Self {
        Self {
            id: model.id.clone(),
            provider_id: model.provider_id.clone(),
            variant: model.variant.clone(),
        }
    }

    pub fn from_selection(selection: &ModelSelection) -> Self {
        Self {
            id: selection.model_id.clone(),
            provider_id: selection.provider_id.clone(),
            variant: selection.variant.clone(),
        }
    }
}

impl Project {
    pub fn from_info(info: &protocol::ProjectInfo) -> Self {
        Self {
            worktree: info.canonical.clone(),
            name: info
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        }
    }
}

/// A change to the root-session list carried by a v2 `session.*` event.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionChange {
    Created(Session),
    Renamed {
        id: String,
        title: String,
    },
    Moved {
        id: String,
        directory: String,
    },
    /// `session.model.selected`, from this client or another one.
    ModelSelected {
        id: String,
        model: SessionModel,
    },
    Deleted(String),
}

impl SessionChange {
    /// `payload` is the whole `/api/event` event (`{id, created, type, location?, data}`).
    pub fn from_event(payload: &Value) -> Option<Self> {
        let event = protocol::Event::deserialize(payload).ok()?;
        match protocol::decode_event(&event) {
            protocol::EventKind::SessionCreated(data) => Some(Self::Created(
                Session::from_created(&data, event.created.map(millis).unwrap_or(0)),
            )),
            protocol::EventKind::SessionRenamed(data) => Some(Self::Renamed {
                id: data.session_id,
                title: display_title(Some(&data.title)),
            }),
            protocol::EventKind::SessionMoved(data) if !data.location.directory.is_empty() => {
                Some(Self::Moved {
                    id: data.session_id,
                    directory: data.location.directory,
                })
            }
            protocol::EventKind::SessionModelSelected(data) => Some(Self::ModelSelected {
                model: SessionModel::from_ref(&data.model),
                id: data.session_id,
            }),
            protocol::EventKind::SessionDeleted(data) => Some(Self::Deleted(data.session_id)),
            _ => None,
        }
    }

    pub fn session_id(&self) -> &str {
        match self {
            Self::Created(session) => &session.id,
            Self::Renamed { id, .. }
            | Self::Moved { id, .. }
            | Self::ModelSelected { id, .. }
            | Self::Deleted(id) => id,
        }
    }

    /// Applies the change to a root-session list and reports whether it changed.
    /// A creation never replaces a known session, whose data is at least as new;
    /// child sessions are ignored; renames and moves of unknown sessions are no-ops.
    pub fn apply(&self, sessions: &mut Vec<Session>) -> bool {
        match self {
            Self::Created(session) => {
                if session.parent_id.is_some()
                    || sessions.iter().any(|existing| existing.id == session.id)
                {
                    return false;
                }
                sessions.push(session.clone());
                true
            }
            Self::Renamed { id, title } => sessions
                .iter_mut()
                .find(|session| &session.id == id && &session.title != title)
                .map(|session| session.title.clone_from(title))
                .is_some(),
            Self::Moved { id, directory } => sessions
                .iter_mut()
                .find(|session| &session.id == id && &session.directory != directory)
                .map(|session| session.directory.clone_from(directory))
                .is_some(),
            Self::ModelSelected { id, model } => sessions
                .iter_mut()
                .find(|session| &session.id == id && session.model.as_ref() != Some(model))
                .map(|session| session.model = Some(model.clone()))
                .is_some(),
            Self::Deleted(id) => {
                let before = sessions.len();
                sessions.retain(|session| &session.id != id);
                sessions.len() != before
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fixture_body(name: &str) -> Value {
        let path = format!(
            "{}/tests/fixtures/v2-2.0.8/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
        serde_json::from_str::<Value>(&text).unwrap()["response"]["body"].take()
    }

    fn selection(provider: &str, model: &str, variant: Option<&str>) -> ModelSelection {
        ModelSelection {
            provider_id: provider.into(),
            model_id: model.into(),
            variant: variant.map(str::to_owned),
        }
    }

    #[test]
    fn catalog_builds_from_the_captured_model_routes() {
        let list: protocol::ModelListResponse =
            serde_json::from_value(fixture_body("model.list")).unwrap();
        let default: protocol::ModelDefaultResponse =
            serde_json::from_value(fixture_body("model.default")).unwrap();
        let catalog = ModelCatalog::from_models(&list.data, default.data.as_ref());

        assert_eq!(
            catalog.models,
            [
                ModelOption {
                    provider_id: "mock".into(),
                    model_id: "mock-model".into(),
                    label: "mock / Mock Model".into(),
                    variants: vec!["low".into(), "medium".into(), "high".into()],
                    supports_attachments: true,
                    context_limit: Some(128_000),
                },
                ModelOption {
                    provider_id: "mock".into(),
                    model_id: "mock-model-alt".into(),
                    label: "mock / Mock Model Alt".into(),
                    variants: vec!["low".into(), "high".into()],
                    supports_attachments: false,
                    context_limit: Some(32_000),
                },
            ]
        );
        assert_eq!(
            catalog.preferred,
            Some(selection("mock", "mock-model", None))
        );
        let debug = format!("{catalog:?}");
        assert!(fixture_body("model.list")
            .to_string()
            .contains("dummy-not-a-secret"));
        assert!(!debug.contains("dummy-not-a-secret"), "{debug}");
        assert!(!debug.contains("baseURL") && !debug.contains("reasoning"));
    }

    #[test]
    fn catalog_uses_catalog_ids_and_skips_disabled_and_deprecated_models() {
        let models: Vec<protocol::ModelInfo> = serde_json::from_value(json!([
            {
                "id": "gpt-6-fast", "modelID": "gpt-6", "providerID": "openai", "name": "GPT 6 Fast",
                "settings": { "apiKey": "sk-hidden" },
                "headers": { "authorization": "Bearer sk-hidden" },
                "capabilities": { "tools": true, "input": ["text", "pdf"], "output": ["text"] },
                "variants": [
                    { "id": "xhigh", "settings": {} }, { "id": "Minimal" }, { "id": "custom" },
                    { "id": "medium", "body": { "k": "sk-hidden" } }
                ],
                "limit": { "context": 400000, "output": 1 }, "status": "active", "enabled": true
            },
            { "id": "old", "modelID": "old", "providerID": "openai", "name": "Old", "status": "deprecated" },
            { "id": "off", "modelID": "off", "providerID": "openai", "name": "Off", "enabled": false },
            {
                "id": "text-only", "modelID": "t", "providerID": "anthropic", "name": "",
                "capabilities": { "input": ["text"] }, "limit": { "context": 0 }, "status": "beta"
            },
            { "id": "defaults", "modelID": "d", "providerID": "zed", "name": "Defaults" }
        ]))
        .unwrap();
        let catalog = ModelCatalog::from_models(&models, Some(&models[0]));

        let ids: Vec<_> = catalog.models.iter().map(|m| m.model_id.as_str()).collect();
        assert_eq!(
            ids,
            ["text-only", "gpt-6-fast", "defaults"],
            "sorted by label"
        );
        let text_only = &catalog.models[0];
        assert_eq!(text_only.label, "anthropic / text-only");
        assert!(!text_only.supports_attachments);
        assert_eq!(text_only.context_limit, None);
        let fast = &catalog.models[1];
        assert_eq!(fast.variants, ["Minimal", "medium", "xhigh", "custom"]);
        assert!(fast.supports_attachments, "pdf input accepts attachments");
        assert_eq!(fast.context_limit, Some(400_000));
        assert!(
            catalog.models[2].supports_attachments,
            "missing capabilities default to text and image"
        );
        assert_eq!(
            catalog.preferred,
            Some(selection("openai", "gpt-6-fast", None)),
            "the default uses ModelInfo.id, not modelID"
        );
        assert!(!format!("{catalog:?}").contains("sk-hidden"));

        // A disabled or missing default falls back to the first model.
        let fallback = ModelCatalog::from_models(&models, Some(&models[2]));
        assert_eq!(
            fallback.preferred,
            Some(selection("anthropic", "text-only", None))
        );
        assert_eq!(
            ModelCatalog::from_models(&models, None).preferred,
            fallback.preferred
        );
        assert_eq!(
            ModelCatalog::from_models(&[], None),
            ModelCatalog::default()
        );
    }

    #[test]
    fn displayed_model_prefers_a_pending_pick_then_the_session_model() {
        let catalog = ModelCatalog {
            models: Vec::new(),
            preferred: Some(selection("p", "default", None)),
        };
        let mut session = listed("ses_a", "A");
        assert_eq!(
            displayed_model(None, Some(&session), &catalog),
            Some(selection("p", "default", None)),
            "a session without a model shows the server default"
        );
        session.model = Some(SessionModel {
            id: "saved".into(),
            provider_id: "p".into(),
            variant: Some("high".into()),
        });
        assert_eq!(
            displayed_model(None, Some(&session), &catalog),
            Some(selection("p", "saved", Some("high")))
        );
        let pending = selection("q", "picked", None);
        assert_eq!(
            displayed_model(Some(&pending), Some(&session), &catalog),
            Some(pending.clone())
        );
        assert_eq!(displayed_model(None, None, &ModelCatalog::default()), None);
    }

    #[test]
    fn only_a_pick_that_changes_the_display_requests_a_switch() {
        let shown_default = selection("p", "default", None);
        assert_eq!(
            model_switch_for_pick(Some(&shown_default), shown_default.clone()),
            None,
            "re-picking the displayed default keeps following the server default"
        );
        assert_eq!(
            model_switch_for_pick(
                Some(&shown_default),
                selection("p", "default", Some("high"))
            ),
            Some(selection("p", "default", Some("high")))
        );
        assert_eq!(
            model_switch_for_pick(Some(&shown_default), selection("p", "other", None)),
            Some(selection("p", "other", None))
        );
        assert_eq!(
            model_switch_for_pick(None, shown_default.clone()),
            Some(shown_default)
        );
        assert_eq!(
            selection("p", "m", Some("low")).to_ref(),
            protocol::ModelRef {
                id: "m".into(),
                provider_id: "p".into(),
                variant: Some("low".into()),
            }
        );
    }

    #[test]
    fn catalog_invalidation_events_carry_their_location() {
        let event = |event_type: &str, location: Option<&str>| {
            let mut event = json!({ "id": "evt_1", "created": 1, "type": event_type, "data": {} });
            if let Some(directory) = location {
                event["location"] = json!({ "directory": directory });
            }
            event
        };
        for event_type in [
            "model.updated",
            "provider.updated",
            "integration.updated",
            "credential.updated",
            "credential.switched",
            "config.updated",
            "models-dev.refreshed",
        ] {
            assert_eq!(
                CatalogInvalidation::from_event(&event(event_type, Some("/work"))),
                Some(CatalogInvalidation {
                    directory: Some("/work".into()),
                    shutdown: false,
                }),
                "{event_type}"
            );
        }
        assert_eq!(
            CatalogInvalidation::from_event(&event("model.updated", None)),
            Some(CatalogInvalidation {
                directory: None,
                shutdown: false,
            })
        );
        assert_eq!(
            CatalogInvalidation::from_event(&event("location.shutdown", Some("/work"))),
            Some(CatalogInvalidation {
                directory: Some("/work".into()),
                shutdown: true,
            })
        );
        for ignored in [
            "agent.updated",
            "session.renamed",
            "server.instance.disposed",
        ] {
            assert_eq!(CatalogInvalidation::from_event(&event(ignored, None)), None);
        }
    }

    #[test]
    fn renders_and_updates_legacy_messages() {
        let mut conversation = Conversation::default();
        assert!(conversation.apply_event(&json!({
            "type": "message.part.updated",
            "properties": {
                "part": {
                    "id": "part_1",
                    "sessionID": "ses_1",
                    "messageID": "msg_1",
                    "type": "text",
                    "text": "hello"
                }
            }
        })));
        assert_eq!(conversation.rendered_rows(), ["AGENT\nhello"]);

        assert!(conversation.apply_event(&json!({
            "type": "message.part.delta",
            "properties": {
                "messageID": "msg_1",
                "partID": "part_1",
                "field": "text",
                "delta": " world"
            }
        })));
        assert_eq!(conversation.rendered_rows(), ["AGENT\nhello world"]);

        assert!(conversation.apply_event(&json!({
            "type": "message.part.removed",
            "properties": { "messageID": "msg_1", "partID": "part_1" }
        })));
        assert!(conversation.rendered_rows().is_empty());
    }

    /// A captured `GET /api/session/{id}/message` page, reversed into
    /// chronological order the way the client does.
    fn fixture_entries(name: &str) -> Vec<protocol::SessionMessage> {
        let path = format!(
            "{}/tests/fixtures/v2-2.0.8/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
        let exchange: protocol::CapturedExchange = serde_json::from_str(&text).unwrap();
        let mut page: protocol::MessageListResponse = exchange.decode_body().unwrap();
        page.data.reverse();
        page.data
    }

    fn history(name: &str) -> Conversation {
        let mut conversation = Conversation::default();
        conversation.replace_from_api(&fixture_entries(name), None);
        conversation
    }

    fn row_values(conversation: &Conversation) -> Vec<Value> {
        conversation
            .transcript_rows()
            .iter()
            .map(|row| serde_json::from_str(row).unwrap())
            .collect()
    }

    fn entries(values: Vec<Value>) -> Vec<protocol::SessionMessage> {
        values
            .into_iter()
            .map(protocol::SessionMessage::from_value)
            .collect()
    }

    #[test]
    fn transcript_rows_include_image_file_parts() {
        let conversation = history("session.messages.attachment");
        let rows = row_values(&conversation);
        assert_eq!(rows.len(), 2, "idle entries render nothing: {rows:?}");
        assert_eq!(rows[0]["role"], "YOU");
        assert_eq!(
            rows[0]["body"],
            "Describe the attached image. [[scenario:text]]\n\nAttached: pixel.png (image/png)"
        );
        let image = rows[0]["images"][0].as_str().unwrap();
        assert!(
            image.starts_with("data:image/png;base64,iVBORw0KGgo"),
            "{image}"
        );
        assert_eq!(rows[0]["images"].as_array().unwrap().len(), 1);
        assert_eq!(rows[1]["role"], "AGENT");
        assert_eq!(rows[1]["body"], "Hello from the mock provider.");

        let other = entries(vec![json!({
            "id": "msg_u", "type": "user", "time": { "created": 7 }, "text": "",
            "files": [
                { "data": "JVBERi0=", "mime": "application/pdf", "source": { "type": "inline" }, "name": "spec.pdf" },
                { "data": "AA==", "mime": "image/gif", "source": { "type": "inline" } }
            ]
        })]);
        let mut conversation = Conversation::default();
        conversation.replace_from_api(&other, None);
        let row = &row_values(&conversation)[0];
        assert_eq!(
            row["body"],
            "Attached: spec.pdf (application/pdf)\n\nAttached: attachment (image/gif)"
        );
        assert_eq!(row["images"], json!(["data:image/gif;base64,AA=="]));
        assert_eq!(row["time"], 7);
    }

    #[test]
    fn transcript_rows_split_tool_calls_with_their_own_times() {
        let entries = fixture_entries("session.messages.tools");
        let tool_times: Vec<i64> = entries
            .iter()
            .filter_map(|entry| match entry {
                protocol::SessionMessage::Assistant(message) => Some(message),
                _ => None,
            })
            .flat_map(|message| &message.content)
            .filter_map(|item| match item {
                protocol::AssistantContent::Tool(tool) => Some(tool.time.created),
                _ => None,
            })
            .collect();
        let mut conversation = Conversation::default();
        conversation.replace_from_api(&entries, None);
        let rows = row_values(&conversation);
        let bodies: Vec<_> = rows
            .iter()
            .map(|row| row["body"].as_str().unwrap())
            .collect();
        assert_eq!(
            bodies,
            [
                "Read two things at once. [[scenario:tools]]",
                "Running two tools at once.",
                "read · completed — README.md",
                "glob · completed — **/*.txt",
                "Both tool calls finished.",
            ]
        );
        assert_eq!(rows[0]["role"], "YOU");
        assert!(rows[1..].iter().all(|row| row["role"] == "AGENT"));
        assert_eq!(rows[2]["kind"], "tool");
        assert_eq!(rows[3]["kind"], "tool");
        assert_eq!(rows[2]["time"], tool_times[0]);
        assert_eq!(rows[3]["time"], tool_times[1]);
        assert_ne!(
            rows[1]["time"], rows[2]["time"],
            "text uses the message time"
        );

        let keys: Vec<_> = conversation.messages[1]
            .segments
            .iter()
            .map(|segment| segment.key.as_str())
            .collect();
        assert_eq!(
            keys,
            ["text:0", "tool:call_mock_read", "tool:call_mock_glob"]
        );
    }

    #[test]
    fn transcript_rows_split_reasoning_from_replies() {
        let conversation = history("session.messages.reasoning");
        let rows = row_values(&conversation);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[1]["body"],
            "Reasoning\nLet me think about this carefully."
        );
        assert_eq!(rows[1]["kind"], "reasoning");
        assert_eq!(
            rows[1]["time"], 1_790_289_087_309_u64,
            "reasoning time.created"
        );
        assert_eq!(rows[2]["body"], "After thinking, the answer is 42.");
        assert_eq!(rows[2]["kind"], "");
        let keys: Vec<_> = conversation.messages[1]
            .segments
            .iter()
            .map(|segment| segment.key.as_str())
            .collect();
        assert_eq!(keys, ["reasoning:0", "text:0"]);
    }

    #[test]
    fn segment_keys_follow_per_kind_ordinals_and_tool_ids() {
        let conversation = {
            let mut conversation = Conversation::default();
            conversation.replace_from_api(
                &entries(vec![json!({
                    "id": "msg_a", "type": "assistant", "time": { "created": 1 }, "agent": "build",
                    "content": [
                        { "type": "reasoning", "text": "r0" },
                        { "type": "text", "text": "t0" },
                        { "type": "tool", "id": "call_1", "name": "shell", "time": { "created": 2 },
                          "state": { "status": "streaming", "input": "{\"command\":\"ls -la\"}" } },
                        { "type": "hologram" },
                        { "type": "reasoning", "text": "r1" },
                        { "type": "text", "text": "t1" },
                        { "type": "tool", "id": "call_2", "name": "subagent", "time": { "created": 3 },
                          "state": { "status": "error", "input": { "description": "Dig in", "agent": "general" },
                                     "error": { "type": "tool", "message": "child failed" } } }
                    ]
                })]),
                None,
            );
            conversation
        };
        let segments: Vec<_> = conversation.messages[0]
            .segments
            .iter()
            .map(|segment| (segment.key.as_str(), segment.text.as_str()))
            .collect();
        assert_eq!(
            segments,
            [
                ("reasoning:0", "r0"),
                ("text:0", "t0"),
                ("tool:call_1", "shell · running — ls -la"),
                ("reasoning:1", "r1"),
                ("text:1", "t1"),
                ("tool:call_2", "subagent · error — Dig in: child failed"),
            ]
        );

        // A live delta keyed by ordinal lands on the history segment.
        let mut conversation = conversation;
        assert!(conversation.apply_event(&json!({
            "type": "session.text.delta",
            "data": { "sessionID": "ses_1", "assistantMessageID": "msg_a", "ordinal": 1, "delta": "!" }
        })));
        assert_eq!(conversation.messages.len(), 1);
        assert_eq!(conversation.messages[0].segments[4].text, "t1!");
    }

    #[test]
    fn captured_scenarios_project_into_sensible_rows() {
        let expect = |name: &str, expected: &[(&str, &str, &str)]| {
            let rows = row_values(&history(name));
            let actual: Vec<_> = rows
                .iter()
                .map(|row| {
                    (
                        row["role"].as_str().unwrap().to_owned(),
                        row["kind"].as_str().unwrap().to_owned(),
                        row["body"].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            let expected: Vec<_> = expected
                .iter()
                .map(|(role, kind, body)| {
                    ((*role).to_owned(), (*kind).to_owned(), (*body).to_owned())
                })
                .collect();
            assert_eq!(actual, expected, "{name}");
        };
        expect(
            "session.messages.text",
            &[
                ("YOU", "", "Say hello. [[scenario:text]]"),
                ("AGENT", "", "Hello from the mock provider."),
            ],
        );
        expect(
            "session.messages.permission",
            &[
                ("YOU", "", "Run a shell command. [[scenario:permission]]"),
                ("AGENT", "tool", "shell · completed — echo permission-probe"),
                ("AGENT", "", "The shell command completed."),
            ],
        );
        expect(
            "session.messages.subagent-permission",
            &[
                (
                    "YOU",
                    "",
                    "Delegate a shell command. [[scenario:subagent-permission]]",
                ),
                (
                    "AGENT",
                    "tool",
                    "subagent · completed — Mock child shell task",
                ),
                ("AGENT", "", "The child subagent finished."),
            ],
        );
        expect(
            "session.messages.error",
            &[
                ("YOU", "", "Fail please. [[scenario:error]]"),
                ("AGENT", "error", "Mock provider exploded"),
            ],
        );
        expect(
            "session.messages.retry",
            &[
                ("YOU", "", "Fail once then recover. [[scenario:retry]]"),
                ("AGENT", "", "Recovered after a retry."),
            ],
        );
        // The interrupted step fails with `aborted`; only its partial text shows.
        expect(
            "session.messages.interrupt",
            &[
                ("YOU", "", "Stream slowly. [[scenario:slow]]"),
                ("AGENT", "", "slow-0 slow-1 slow-2 "),
            ],
        );

        // A background subagent completes later as a synthetic entry, which
        // renders like assistant text.
        let conversation = history("session.messages.subagent");
        let rows = row_values(&conversation);
        assert_eq!(rows.len(), 5, "{rows:?}");
        assert_eq!(rows[0]["role"], "YOU");
        assert_eq!(rows[1]["body"], "subagent · completed — Mock child task");
        assert_eq!(rows[1]["kind"], "tool");
        assert_eq!(rows[2]["body"], "Launched a background subagent.");
        assert!(rows[3]["body"]
            .as_str()
            .unwrap()
            .contains("Hello from the child subagent."));
        assert_eq!(
            (&rows[3]["role"], &rows[3]["kind"]),
            (&json!("AGENT"), &json!(""))
        );
        assert_eq!(rows[4]["body"], "Hello from the mock provider.");
        assert!(conversation
            .messages
            .iter()
            .all(|message| message.id.starts_with("msg_")));
    }

    #[test]
    fn non_user_entries_render_as_agent_text() {
        let mut conversation = Conversation::default();
        conversation.replace_from_api(
            &entries(vec![
                json!({ "id": "msg_1", "type": "model-switched", "time": { "created": 1 },
                        "model": { "id": "gpt-6", "providerID": "openai", "variant": "high" } }),
                json!({ "id": "msg_2", "type": "agent-switched", "time": { "created": 2 }, "agent": "plan" }),
                json!({ "id": "msg_3", "type": "location-switched", "time": { "created": 3 },
                        "location": { "directory": "/work/b" } }),
                json!({ "id": "msg_4", "type": "system", "time": { "created": 4 },
                        "text": "<all instructions>", "description": "Instructions updated: AGENTS.md" }),
                json!({ "id": "msg_5", "type": "skill", "time": { "created": 5 },
                        "skill": "pdf", "name": "PDF tools", "text": "# Long skill body" }),
                json!({ "id": "msg_6", "type": "shell", "time": { "created": 6, "completed": 7 },
                        "shellID": "sh_1", "command": "ls", "status": "exited", "exit": 0,
                        "output": { "output": "a\nb\n", "cursor": 4, "size": 4, "truncated": false } }),
                json!({ "id": "msg_7", "type": "shell", "time": { "created": 8 },
                        "shellID": "sh_2", "command": "sleep 99", "status": "timeout" }),
                json!({ "id": "msg_8", "type": "compaction", "time": { "created": 9 },
                        "status": "completed", "summary": "Earlier work summary." }),
                json!({ "id": "msg_9", "type": "compaction", "time": { "created": 10 }, "status": "running" }),
                json!({ "id": "msg_10", "type": "compaction", "time": { "created": 11 }, "status": "failed",
                        "error": { "type": "provider", "message": "too long" } }),
                json!({ "id": "msg_11", "type": "synthetic", "time": { "created": 12 }, "text": "Continue." }),
                json!({ "id": "msg_12", "type": "idle", "time": { "created": 13 }, "outcome": "succeeded" }),
                json!({ "id": "msg_13", "type": "teleported", "time": { "created": 14 }, "text": "Beamed up" }),
                json!({ "id": "msg_14", "type": "teleported", "time": { "created": 15 } }),
            ]),
            None,
        );
        let rows = row_values(&conversation);
        let bodies: Vec<_> = rows
            .iter()
            .map(|row| row["body"].as_str().unwrap())
            .collect();
        assert_eq!(
            bodies,
            [
                "Switched model to openai/gpt-6 (high)",
                "Switched agent to plan",
                "Moved to /work/b",
                "Instructions updated: AGENTS.md",
                "Skill: PDF tools",
                "```\n$ ls\na\nb\n```",
                "```\n$ sleep 99\n```\n\nTimed out",
                "Earlier work summary.",
                "Compacting the conversation…",
                "Compaction failed: too long",
                "Continue.",
                "Beamed up",
            ]
        );
        assert!(rows
            .iter()
            .all(|row| row["role"] == "AGENT" && row["kind"] == "" && row["images"] == json!([])));
        assert_eq!(rows[0]["time"], 1);
        assert_eq!(rows[11]["time"], 14);
        assert_eq!(conversation.context_tokens(), None);
    }

    #[test]
    fn shell_output_fence_outgrows_backticks_inside() {
        assert_eq!(code_block("a ``` b"), "````\na ``` b\n````");
        assert_eq!(code_block("plain"), "```\nplain\n```");
    }

    #[test]
    fn paged_history_prepends_older_entries_once() {
        let pages: Vec<_> = (1..=4)
            .map(|index| fixture_entries(&format!("session.messages.page{index}")))
            .collect();
        let mut conversation = Conversation::default();
        conversation.replace_from_api(&pages[0], Some("c1".into()));
        for (index, page) in pages.iter().enumerate().skip(1) {
            conversation.prepend_from_api(page, Some(format!("c{}", index + 1)));
        }
        conversation.prepend_from_api(&pages[1], None);
        assert_eq!(conversation.next_cursor, None);
        let rows = row_values(&conversation);
        let users: Vec<_> = rows
            .iter()
            .filter(|row| row["role"] == "YOU")
            .map(|row| row["body"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(users.len(), 4, "{users:?}");
        let mut sorted = users.clone();
        sorted.sort();
        assert_eq!(users, sorted, "oldest turn first: {users:?}");
        let times: Vec<_> = rows
            .iter()
            .map(|row| row["time"].as_u64().unwrap())
            .collect();
        assert!(times.windows(2).all(|pair| pair[0] <= pair[1]), "{times:?}");
    }

    #[test]
    fn folds_current_text_deltas_without_polling() {
        let mut conversation = Conversation::default();
        for payload in [
            json!({
                "type": "session.next.step.started",
                "properties": { "sessionID": "ses_1", "assistantMessageID": "msg_1" }
            }),
            json!({
                "type": "session.next.text.started",
                "properties": { "sessionID": "ses_1", "assistantMessageID": "msg_1", "textID": "text_1" }
            }),
            json!({
                "type": "session.next.text.delta",
                "properties": { "sessionID": "ses_1", "assistantMessageID": "msg_1", "textID": "text_1", "delta": "hel" }
            }),
            json!({
                "type": "session.next.text.delta",
                "properties": { "sessionID": "ses_1", "assistantMessageID": "msg_1", "textID": "text_1", "delta": "lo" }
            }),
        ] {
            assert!(conversation.apply_event(&payload));
        }

        assert_eq!(conversation.rendered_rows(), ["AGENT\nhello"]);
    }

    #[test]
    fn preserves_session_errors_without_overwriting_messages() {
        let mut conversation = Conversation::default();
        for message in ["first failure", "second failure"] {
            assert!(conversation.apply_event(&json!({
                "type": "session.error",
                "properties": { "sessionID": "ses_1", "error": message }
            })));
        }

        assert_eq!(
            conversation.rendered_rows(),
            [
                "AGENT\nError: first failure",
                "AGENT\nError: second failure"
            ]
        );
    }

    #[test]
    fn extracts_status_from_both_event_generations() {
        assert_eq!(
            event_run_status(&json!({
                "type": "session.status",
                "properties": { "sessionID": "ses_1", "status": { "type": "busy" } }
            })),
            Some(("ses_1".into(), RunStatus::Busy))
        );
        assert_eq!(
            event_run_status(&json!({
                "type": "session.status",
                "properties": {
                    "sessionID": "ses_1",
                    "status": {
                        "type": "retry",
                        "message": "retrying in 4s",
                        "attempt": 1
                    }
                }
            })),
            Some((
                "ses_1".into(),
                RunStatus::Retry {
                    message: "retrying in 4s".into(),
                    attempt: 1,
                }
            ))
        );
        assert_eq!(
            event_run_status(&json!({
                "type": "session.error",
                "properties": { "sessionID": "ses_1" }
            })),
            Some(("ses_1".into(), RunStatus::Idle))
        );
    }

    #[test]
    fn transcript_rows_emits_error_row_for_failed_message() {
        let msg = ChatMessage {
            id: "msg_err".into(),
            role: Role::Assistant,
            created: 100,
            segments: Vec::new(),
            error: Some("AI_APICallError: Not Found".into()),
            context_tokens: None,
        };
        let rows = msg.transcript_rows();
        assert_eq!(rows.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&rows[0]).unwrap();
        assert_eq!(parsed["kind"], "error");
        assert_eq!(parsed["body"], "AI_APICallError: Not Found");
        assert_eq!(parsed["role"], "AGENT");
    }

    #[test]
    fn context_usage_uses_the_latest_assistant_window_tokens() {
        let mut conversation = Conversation::default();
        conversation.replace_from_api(
            &entries(vec![
                json!({ "id": "msg_user", "type": "user", "time": { "created": 1 }, "text": "hi" }),
                json!({ "id": "msg_old", "type": "assistant", "time": { "created": 2 }, "agent": "build",
                        "content": [], "tokens": { "input": 800, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } } }),
                json!({ "id": "msg_new", "type": "assistant", "time": { "created": 3 }, "agent": "build",
                        "content": [], "tokens": { "input": 124, "output": 40, "reasoning": 10.4, "cache": { "read": 48000, "write": 200 } } }),
                json!({ "id": "msg_streaming", "type": "assistant", "time": { "created": 4 }, "agent": "build", "content": [] }),
                json!({ "id": "msg_idle", "type": "idle", "time": { "created": 5 }, "outcome": "succeeded" }),
                json!({ "id": "msg_switch", "type": "model-switched", "time": { "created": 6 },
                        "model": { "id": "m", "providerID": "p" } }),
            ]),
            None,
        );
        assert_eq!(conversation.context_tokens(), Some(48_374));
        assert!(conversation.apply_event(&json!({
            "type": "message.updated",
            "properties": {
                "info": {
                    "id": "msg_new",
                    "role": "assistant",
                    "time": { "created": 3 },
                    "tokens": {
                        "input": 200,
                        "output": 50,
                        "cache": { "read": 50000, "write": 0 }
                    }
                }
            }
        })));
        assert_eq!(conversation.context_tokens(), Some(50_250));
        assert_eq!(history("session.messages.long").context_tokens(), Some(342));
    }

    #[test]
    fn compact_context_usage_matches_the_composer_label() {
        assert_eq!(format_context_usage(0, 200_000), "0 / 200k");
        assert_eq!(format_context_usage(12_400, 200_000), "12.4k / 200k");
        assert_eq!(format_context_usage(999, 8_192), "999 / 8.2k");
        assert_eq!(format_context_usage(1_500_000, 2_000_000), "1.5m / 2m");
    }

    #[test]
    fn running_tool_extracts_command_from_input() {
        let tool: protocol::ToolCall = serde_json::from_value(json!({
            "type": "tool",
            "id": "call_1",
            "name": "shell",
            "time": { "created": 5 },
            "state": {
                "status": "running",
                "input": {
                    "command": "pnpm exec playwright test\n  e2e/tests/desktop/apps20-ad-resizer.spec.ts",
                    "description": "Run the resizer spec"
                },
                "metadata": {}
            }
        }))
        .unwrap();
        let segment = tool_segment(&tool);
        assert_eq!(
            segment.text,
            "shell · running — pnpm exec playwright test e2e/tests/desktop/apps20-ad-resizer.spec.ts"
        );
        assert_eq!((segment.key.as_str(), segment.created), ("tool:call_1", 5));
    }

    #[test]
    fn tool_titles_follow_the_v2_tool_inputs() {
        let title = |name: &str, input: Value| tool_title(name, input.as_object().unwrap());
        assert_eq!(
            title("shell", json!({ "description": "d" })).as_deref(),
            Some("d")
        );
        assert_eq!(
            title(
                "subagent",
                json!({ "agent": "general", "description": "Mock task", "prompt": "p" })
            )
            .as_deref(),
            Some("Mock task")
        );
        for name in ["read", "write", "edit"] {
            assert_eq!(
                title(name, json!({ "path": "src/a.rs" })).as_deref(),
                Some("src/a.rs")
            );
        }
        assert_eq!(
            title("grep", json!({ "pattern": "fn main", "path": "src" })).as_deref(),
            Some("fn main")
        );
        assert_eq!(
            title("glob", json!({ "pattern": "**/*.rs" })).as_deref(),
            Some("**/*.rs")
        );
        assert_eq!(
            title(
                "webfetch",
                json!({ "url": "https://x.dev", "format": "markdown" })
            )
            .as_deref(),
            Some("https://x.dev")
        );
        assert_eq!(
            title("websearch", json!({ "query": "gtk4" })).as_deref(),
            Some("gtk4")
        );
        assert_eq!(
            title("skill", json!({ "id": "pdf" })).as_deref(),
            Some("pdf")
        );
        assert_eq!(
            title("kagi_search", json!({ "query": "rust" })).as_deref(),
            Some("rust")
        );
        assert_eq!(
            title("patch", json!({ "patchText": "*** Begin Patch" })),
            None
        );
        assert_eq!(title("shell", json!({ "command": "  " })), None);
    }

    fn session_info(value: Value) -> protocol::SessionInfo {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn session_info_maps_to_a_session() {
        let session = Session::from_info(&session_info(json!({
            "id": "ses_a",
            "parentID": "ses_parent",
            "projectID": "prj",
            "agent": "build",
            "model": { "id": "gpt-6", "providerID": "openai", "variant": "high" },
            "time": { "created": 10, "updated": 20, "archived": 30 },
            "title": "Fix the build",
            "location": { "directory": "/work/a" }
        })));
        assert_eq!(
            session,
            Session {
                id: "ses_a".into(),
                directory: "/work/a".into(),
                title: "Fix the build".into(),
                time: SessionTime {
                    created: 10,
                    updated: 20,
                    archived: Some(30.0),
                },
                parent_id: Some("ses_parent".into()),
                model: Some(SessionModel {
                    id: "gpt-6".into(),
                    provider_id: "openai".into(),
                    variant: Some("high".into()),
                }),
            }
        );
    }

    #[test]
    fn untitled_session_info_gets_a_stable_title() {
        for title in [json!(null), json!(""), json!("   ")] {
            let session = Session::from_info(&session_info(json!({
                "id": "ses_a",
                "time": { "created": 1, "updated": 2 },
                "title": title,
                "location": { "directory": "/work" }
            })));
            assert_eq!(session.title, "Untitled session");
            assert_eq!(session.parent_id, None);
            assert_eq!(session.model, None);
            assert_eq!(session.time.archived, None);
        }
    }

    #[test]
    fn project_info_maps_canonical_to_worktree() {
        let project: protocol::ProjectInfo = serde_json::from_value(json!({
            "id": "prj",
            "canonical": "/work/a",
            "name": "A",
            "sandboxes": ["/work/a-sandbox"]
        }))
        .unwrap();
        assert_eq!(
            Project::from_info(&project),
            Project {
                worktree: "/work/a".into(),
                name: Some("A".into()),
            }
        );
        let unnamed: protocol::ProjectInfo =
            serde_json::from_value(json!({ "id": "prj", "canonical": "/work/b", "name": "" }))
                .unwrap();
        assert_eq!(Project::from_info(&unnamed).name, None);
    }

    fn session_event(event_type: &str, data: Value) -> Value {
        json!({
            "id": "evt_1",
            "created": 500,
            "type": event_type,
            "location": { "directory": "/work" },
            "data": data
        })
    }

    fn listed(id: &str, title: &str) -> Session {
        Session {
            id: id.into(),
            directory: "/work".into(),
            title: title.into(),
            time: SessionTime {
                created: 1,
                updated: 900,
                archived: None,
            },
            parent_id: None,
            model: None,
        }
    }

    #[test]
    fn session_created_event_builds_a_root_session_from_creation_fields() {
        let change = SessionChange::from_event(&session_event(
            "session.created",
            json!({
                "sessionID": "ses_new",
                "slug": "misty-garden",
                "version": "2.0.8",
                "projectID": "prj",
                "location": { "directory": "/work/new" },
                "subpath": ""
            }),
        ))
        .unwrap();
        let SessionChange::Created(session) = &change else {
            panic!("unexpected {change:?}");
        };
        assert_eq!(session.id, "ses_new");
        assert_eq!(session.directory, "/work/new");
        assert_eq!(session.title, "Untitled session");
        assert_eq!((session.time.created, session.time.updated), (500, 500));

        let mut sessions = vec![listed("ses_a", "A")];
        assert!(change.apply(&mut sessions));
        assert_eq!(sessions.len(), 2);
        assert!(!change.apply(&mut sessions), "a creation is idempotent");

        let mut known = vec![listed("ses_new", "Server title")];
        assert!(!change.apply(&mut known), "never replaces a known session");
        assert_eq!(known[0].title, "Server title");

        let child = SessionChange::from_event(&session_event(
            "session.created",
            json!({
                "sessionID": "ses_child",
                "parentID": "ses_a",
                "location": { "directory": "/work" }
            }),
        ))
        .unwrap();
        let mut sessions = vec![listed("ses_a", "A")];
        assert!(!child.apply(&mut sessions));
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn session_rename_move_and_delete_events_update_the_list() {
        let mut sessions = vec![listed("ses_a", "A"), listed("ses_b", "B")];

        let renamed = SessionChange::from_event(&session_event(
            "session.renamed",
            json!({ "sessionID": "ses_a", "title": "Renamed" }),
        ))
        .unwrap();
        assert_eq!(renamed.session_id(), "ses_a");
        assert!(renamed.apply(&mut sessions));
        assert_eq!(sessions[0].title, "Renamed");
        assert!(!renamed.apply(&mut sessions));

        let moved = SessionChange::from_event(&session_event(
            "session.moved",
            json!({
                "sessionID": "ses_b",
                "location": { "directory": "/work/elsewhere" },
                "projectID": "prj"
            }),
        ))
        .unwrap();
        assert!(moved.apply(&mut sessions));
        assert_eq!(sessions[1].directory, "/work/elsewhere");

        let unknown = SessionChange::Renamed {
            id: "ses_missing".into(),
            title: "X".into(),
        };
        assert!(!unknown.apply(&mut sessions));

        let deleted = SessionChange::from_event(&session_event(
            "session.deleted",
            json!({ "sessionID": "ses_a" }),
        ))
        .unwrap();
        assert_eq!(deleted, SessionChange::Deleted("ses_a".into()));
        assert!(deleted.apply(&mut sessions));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "ses_b");

        for ignored in [
            session_event("session.model.selected", json!({ "sessionID": "ses_b" })),
            session_event("session.renamed", json!({})),
            json!({ "type": "session.deleted", "data": { "sessionID": "ses_b" } }),
        ] {
            assert_eq!(SessionChange::from_event(&ignored), None, "{ignored}");
        }
    }

    #[test]
    fn session_model_selected_updates_the_saved_model() {
        let mut sessions = vec![listed("ses_a", "A")];
        let selected = SessionChange::from_event(&session_event(
            "session.model.selected",
            json!({
                "sessionID": "ses_a",
                "model": { "id": "alt", "providerID": "mock", "variant": "high" },
                "previous": { "id": "base", "providerID": "mock" }
            }),
        ))
        .unwrap();
        assert_eq!(selected.session_id(), "ses_a");
        assert!(selected.apply(&mut sessions));
        assert_eq!(
            sessions[0].model_selection(),
            Some(selection("mock", "alt", Some("high")))
        );
        assert!(!selected.apply(&mut sessions), "an echo changes nothing");
        assert!(!SessionChange::ModelSelected {
            id: "ses_missing".into(),
            model: SessionModel::from_selection(&selection("mock", "alt", None)),
        }
        .apply(&mut sessions));
        assert_eq!(
            SessionChange::from_event(&session_event(
                "session.agent.selected",
                json!({ "sessionID": "ses_a", "agent": "plan" }),
            )),
            None,
            "agent selections are tolerated and ignored"
        );
    }

    #[test]
    fn captured_session_lifecycle_events_decode() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/v2-2.0.8/events/session-lifecycle.jsonl"
        );
        let text = std::fs::read_to_string(path).expect("captured session-lifecycle events");
        let mut sessions = Vec::new();
        let mut created = 0;
        let mut renamed = 0;
        let mut model_selected = 0;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let payload: Value = serde_json::from_str(line).unwrap();
            match SessionChange::from_event(&payload) {
                Some(change @ SessionChange::Created(_)) => {
                    created += 1;
                    assert!(change.apply(&mut sessions));
                }
                Some(change @ SessionChange::Renamed { .. }) => {
                    renamed += 1;
                    assert!(change.apply(&mut sessions));
                }
                Some(change @ SessionChange::ModelSelected { .. }) => {
                    model_selected += 1;
                    assert!(change.apply(&mut sessions));
                }
                Some(other) => panic!("unexpected {other:?}"),
                None => {}
            }
        }
        assert!(created > 0 && renamed > 0 && model_selected > 0);
        let switched: protocol::SessionResponse =
            serde_json::from_value(fixture_body("session.get.modelSwitched")).unwrap();
        let switched = Session::from_info(&switched.data);
        assert_eq!(
            sessions
                .iter()
                .find(|session| session.id == switched.id)
                .and_then(|session| session.model.as_ref()),
            switched.model.as_ref(),
            "the event and the refetched session agree on the model"
        );
        assert!(sessions
            .iter()
            .all(|session| !session.directory.is_empty() && session.time.created > 0));
        assert!(sessions
            .iter()
            .any(|session| session.title != "Untitled session"));
    }
}
