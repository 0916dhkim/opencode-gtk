use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Session {
    pub id: String,
    pub directory: String,
    pub title: String,
    pub time: SessionTime,
    #[serde(default, rename = "parentID")]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
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

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct ModelSelection {
    pub provider_id: String,
    pub model_id: String,
    pub variant: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOption {
    pub provider_id: String,
    pub model_id: String,
    pub label: String,
    pub variants: Vec<String>,
    pub supports_attachments: bool,
    pub context_limit: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelCatalog {
    pub models: Vec<ModelOption>,
    pub preferred: Option<ModelSelection>,
}

impl ModelCatalog {
    pub fn from_values(providers: &Value, config: &Value) -> Self {
        let configured = config
            .get("model")
            .and_then(Value::as_str)
            .and_then(split_model_id);
        let defaults = providers.get("default").and_then(Value::as_object);
        let mut models = Vec::new();

        for provider in providers
            .get("providers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(provider_id) = provider.get("id").and_then(Value::as_str) else {
                continue;
            };
            let provider_name = provider
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(provider_id);
            let Some(provider_models) = provider.get("models").and_then(Value::as_object) else {
                continue;
            };

            for (map_id, model) in provider_models {
                if model.get("status").and_then(Value::as_str) == Some("deprecated") {
                    continue;
                }
                let model_id = model
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(map_id)
                    .to_owned();
                let model_name = model
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&model_id)
                    .to_owned();
                let mut variants = variants(model);
                variants.sort_by_key(|variant| variant_rank(variant));
                let supports_attachments = model
                    .pointer("/capabilities/attachment")
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| {
                        ["audio", "image", "video", "pdf"].iter().any(|kind| {
                            model
                                .pointer(&format!("/capabilities/input/{kind}"))
                                .and_then(Value::as_bool)
                                .unwrap_or(false)
                        })
                    });

                models.push(ModelOption {
                    provider_id: provider_id.to_owned(),
                    model_id,
                    label: format!("{provider_name} / {model_name}"),
                    variants,
                    supports_attachments,
                    context_limit: model_context_limit(model),
                });
            }
        }

        models.sort_by(|left, right| left.label.to_lowercase().cmp(&right.label.to_lowercase()));
        let preferred = configured
            .filter(|selection| contains_model(&models, selection))
            .or_else(|| {
                providers
                    .get("providers")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .find_map(|provider| {
                        let provider_id = provider.get("id")?.as_str()?;
                        let model_id = defaults?.get(provider_id)?.as_str()?;
                        let selection = ModelSelection {
                            provider_id: provider_id.to_owned(),
                            model_id: model_id.to_owned(),
                            variant: None,
                        };
                        contains_model(&models, &selection).then_some(selection)
                    })
            })
            .or_else(|| {
                models.first().map(|model| ModelSelection {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                    variant: None,
                })
            });

        Self { models, preferred }
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

fn split_model_id(value: &str) -> Option<ModelSelection> {
    let (provider_id, model_id) = value.split_once('/')?;
    Some(ModelSelection {
        provider_id: provider_id.to_owned(),
        model_id: model_id.to_owned(),
        variant: None,
    })
}

fn variants(model: &Value) -> Vec<String> {
    match model.get("variants") {
        Some(Value::Object(values)) => values.keys().cloned().collect(),
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|value| {
                value
                    .as_str()
                    .or_else(|| value.get("id").and_then(Value::as_str))
                    .map(str::to_owned)
            })
            .collect(),
        _ => Vec::new(),
    }
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

fn model_context_limit(model: &Value) -> Option<u64> {
    json_u64(model.pointer("/limit/context"))
        .or_else(|| json_u64(model.get("context")))
        .or_else(|| json_u64(model.get("contextWindow")))
        .or_else(|| json_u64(model.get("context_window")))
        .filter(|limit| *limit > 0)
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
    pub fn replace_from_api(&mut self, envelopes: &[Value], next_cursor: Option<String>) {
        self.messages = envelopes.iter().filter_map(message_from_api).collect();
        self.next_cursor = next_cursor;
        self.loaded = true;
    }

    pub fn prepend_from_api(&mut self, envelopes: &[Value], next_cursor: Option<String>) {
        let existing: HashMap<_, _> = self
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| (message.id.as_str(), index))
            .collect();
        let mut earlier: Vec<_> = envelopes
            .iter()
            .filter_map(message_from_api)
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
        let detail = data
            .get("error")
            .and_then(|error| error_text(Some(error)))
            .map(|error| format!(": {error}"))
            .unwrap_or_default();
        self.messages[index].upsert_segment(Segment {
            key,
            kind: SegmentKind::Tool,
            text: format!("{name} · {status}{detail}"),
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

fn message_from_api(envelope: &Value) -> Option<ChatMessage> {
    let info = envelope.get("info")?;
    let role = match info.get("role")?.as_str()? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    let mut message = ChatMessage {
        id: info.get("id")?.as_str()?.to_owned(),
        role,
        created: info
            .pointer("/time/created")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        segments: Vec::new(),
        error: error_text(info.get("error")),
        context_tokens: message_context_tokens(info),
    };
    for part in envelope
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(segment) = segment_from_part(part) {
            message.segments.push(segment);
        }
    }
    Some(message)
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
            let title = part
                .pointer("/state/title")
                .and_then(Value::as_str)
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

pub fn event_session(payload: &Value) -> Option<Session> {
    match payload.get("type")?.as_str()? {
        "session.created" | "session.updated" => {
            serde_json::from_value(event_data(payload).get("info")?.clone()).ok()
        }
        _ => None,
    }
}

pub fn deleted_session_id(payload: &Value) -> Option<&str> {
    (payload.get("type").and_then(Value::as_str) == Some("session.deleted"))
        .then(|| {
            event_data(payload)
                .pointer("/info/id")
                .and_then(Value::as_str)
        })
        .flatten()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn catalog_uses_configured_model_and_server_variants() {
        let providers = json!({
            "providers": [{
                "id": "openai",
                "name": "OpenAI",
                "models": {
                    "gpt-5.6": {
                        "id": "gpt-5.6",
                        "name": "GPT 5.6",
                        "status": "active",
                        "capabilities": { "attachment": true },
                        "limit": { "context": 200000, "output": 8192 },
                        "variants": { "high": {}, "low": {}, "medium": {} }
                    }
                }
            }],
            "default": { "openai": "gpt-5.6" }
        });
        let catalog = ModelCatalog::from_values(&providers, &json!({ "model": "openai/gpt-5.6" }));

        assert_eq!(catalog.models.len(), 1);
        assert_eq!(catalog.models[0].variants, ["low", "medium", "high"]);
        assert!(catalog.models[0].supports_attachments);
        assert_eq!(catalog.models[0].context_limit, Some(200_000));
        assert_eq!(
            catalog.preferred,
            Some(ModelSelection {
                provider_id: "openai".into(),
                model_id: "gpt-5.6".into(),
                variant: None,
            })
        );
    }

    #[test]
    fn renders_and_updates_legacy_messages() {
        let mut conversation = Conversation::default();
        conversation.replace_from_api(
            &[json!({
                "info": {
                    "id": "msg_1",
                    "sessionID": "ses_1",
                    "role": "assistant",
                    "time": { "created": 1 }
                },
                "parts": [{
                    "id": "part_1",
                    "sessionID": "ses_1",
                    "messageID": "msg_1",
                    "type": "text",
                    "text": "hel"
                }]
            })],
            None,
        );

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

    #[test]
    fn transcript_rows_include_image_file_parts() {
        let mut conversation = Conversation::default();
        conversation.replace_from_api(
            &[json!({
                "info": {
                    "id": "msg_image",
                    "sessionID": "ses_1",
                    "role": "user",
                    "time": { "created": 1 }
                },
                "parts": [{
                    "id": "part_image",
                    "sessionID": "ses_1",
                    "messageID": "msg_image",
                    "type": "file",
                    "filename": "clipboard.png",
                    "mime": "image/png",
                    "url": "data:image/png;base64,AA=="
                }]
            })],
            None,
        );

        let row: Value = serde_json::from_str(&conversation.transcript_rows()[0]).unwrap();
        assert_eq!(row["role"], "YOU");
        assert_eq!(row["time"], 1);
        assert_eq!(row["images"], json!(["data:image/png;base64,AA=="]));
        assert_eq!(
            conversation.rendered_rows(),
            ["YOU\nAttached: clipboard.png (image/png)"]
        );
    }

    #[test]
    fn transcript_rows_split_tool_calls_with_their_own_times() {
        let mut conversation = Conversation::default();
        conversation.replace_from_api(
            &[
                json!({
                    "info": {
                        "id": "msg_user",
                        "sessionID": "ses_1",
                        "role": "user",
                        "time": { "created": 1_704_067_200_000_u64 }
                    },
                    "parts": [{
                        "id": "part_user",
                        "type": "text",
                        "text": "run it"
                    }]
                }),
                json!({
                    "info": {
                        "id": "msg_assistant",
                        "sessionID": "ses_1",
                        "role": "assistant",
                        "time": { "created": 1_704_067_260_000_u64 }
                    },
                    "parts": [
                        {
                            "id": "part_text",
                            "type": "text",
                            "text": "calling bash"
                        },
                        {
                            "id": "part_tool",
                            "type": "tool",
                            "tool": "bash",
                            "state": {
                                "status": "completed",
                                "time": { "start": 1_704_067_261_000_u64 }
                            }
                        }
                    ]
                }),
            ],
            None,
        );

        let rows: Vec<Value> = conversation
            .transcript_rows()
            .iter()
            .map(|row| serde_json::from_str(row).unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["role"], "YOU");
        assert_eq!(rows[0]["body"], "run it");
        assert_eq!(rows[0]["time"], 1_704_067_200_000_u64);
        assert_eq!(rows[1]["body"], "calling bash");
        assert_eq!(rows[1]["time"], 1_704_067_260_000_u64);
        assert_eq!(rows[2]["body"], "bash · completed");
        assert_eq!(rows[2]["time"], 1_704_067_261_000_u64);
        assert_eq!(rows[2]["kind"], "tool");
    }

    #[test]
    fn transcript_rows_split_reasoning_from_replies() {
        let mut conversation = Conversation::default();
        conversation.replace_from_api(
            &[json!({
                "info": {
                    "id": "msg_assistant",
                    "sessionID": "ses_1",
                    "role": "assistant",
                    "time": { "created": 2 }
                },
                "parts": [
                    {
                        "id": "part_reason",
                        "type": "reasoning",
                        "text": "think first"
                    },
                    {
                        "id": "part_text",
                        "type": "text",
                        "text": "done"
                    }
                ]
            })],
            None,
        );

        let rows: Vec<Value> = conversation
            .transcript_rows()
            .iter()
            .map(|row| serde_json::from_str(row).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["body"], "Reasoning\nthink first");
        assert_eq!(rows[0]["kind"], "reasoning");
        assert_eq!(rows[1]["body"], "done");
        assert_eq!(rows[1]["kind"], "");
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
            &[
                json!({
                    "info": {
                        "id": "msg_user",
                        "role": "user",
                        "time": { "created": 1 }
                    },
                    "parts": []
                }),
                json!({
                    "info": {
                        "id": "msg_old",
                        "role": "assistant",
                        "time": { "created": 2 },
                        "tokens": { "input": 800 }
                    },
                    "parts": []
                }),
                json!({
                    "info": {
                        "id": "msg_new",
                        "role": "assistant",
                        "time": { "created": 3 },
                        "tokens": {
                            "input": 124,
                            "output": 40,
                            "reasoning": 10,
                            "cache": { "read": 48000, "write": 200 }
                        }
                    },
                    "parts": []
                }),
            ],
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
    }

    #[test]
    fn compact_context_usage_matches_the_composer_label() {
        assert_eq!(format_context_usage(0, 200_000), "0 / 200k");
        assert_eq!(format_context_usage(12_400, 200_000), "12.4k / 200k");
        assert_eq!(format_context_usage(999, 8_192), "999 / 8.2k");
        assert_eq!(format_context_usage(1_500_000, 2_000_000), "1.5m / 2m");
    }
}
