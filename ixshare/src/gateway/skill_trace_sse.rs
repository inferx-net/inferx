// Shared SSE parsing types used by both skill_chain and mcp_stream_server.

use serde::{Deserialize, Serialize};

#[derive(Default)]
pub(super) struct SseParser {
    pending_event: Option<String>,
    data_lines: Vec<String>,
}

pub(super) enum SseMessage {
    Event { event: Option<String>, data: String },
    Comment(String),
}

impl SseParser {
    pub(super) fn push_line(&mut self, line: &str) -> Option<SseMessage> {
        if let Some(rest) = line.strip_prefix(':') {
            return Some(SseMessage::Comment(rest.trim().to_string()));
        }
        if line.is_empty() {
            let data = self.data_lines.join("\n");
            let event = self.pending_event.take();
            self.data_lines.clear();
            if data.is_empty() && event.is_none() {
                return None;
            }
            return Some(SseMessage::Event { event, data });
        }
        if let Some(rest) = line.strip_prefix("event:") {
            self.pending_event = Some(rest.trim().to_string());
            return None;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            self.data_lines.push(rest.trim().to_string());
        }
        None
    }
}

/// Full trace event payload emitted by the gateway.
/// Both skill_chain (all fields) and mcp_stream_server (subset) deserialize from this.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct SkillTraceEventPayload {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub call_id: String,
    #[serde(default)]
    pub parent_call_id: Option<String>,
    #[serde(default)]
    pub depth: u32,
    pub skill: String,
    #[serde(default)]
    pub ts_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}
