//! SSE renderers, one module per wire dialect.
//!
//! Each renderer turns a canonical [`crate::Reply`] into an ordered list of
//! [`SseFrame`]s the server writes as chunked `text/event-stream` output. M1
//! ships [`openai`] only; `anthropic` (M3) and `codex` (M4) slot in here
//! alongside it without touching the server.

mod openai;

pub use openai::render_openai;

/// One Server-Sent Events frame: an optional `event:` line plus the `data:`
/// payload. OpenAI uses `data:`-only frames (`event` is `None`); Anthropic and
/// Codex set `event` for their typed streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// The `event:` name, or `None` for a `data:`-only frame.
    pub event: Option<String>,
    /// The `data:` payload (a JSON string, or the sentinel `[DONE]`).
    pub data: String,
}

impl SseFrame {
    /// A `data:`-only frame (no `event:` line), as used by the OpenAI dialect.
    pub fn data(data: impl Into<String>) -> Self {
        SseFrame {
            event: None,
            data: data.into(),
        }
    }

    /// Serialize this frame to wire bytes, including the trailing blank line
    /// that terminates an SSE event.
    pub fn to_wire(&self) -> String {
        let mut out = String::new();
        if let Some(event) = &self.event {
            out.push_str("event: ");
            out.push_str(event);
            out.push('\n');
        }
        out.push_str("data: ");
        out.push_str(&self.data);
        out.push_str("\n\n");
        out
    }
}

/// Concatenate frames into a single `text/event-stream` body.
pub fn frames_to_body(frames: &[SseFrame]) -> String {
    frames.iter().map(SseFrame::to_wire).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_only_frame_has_no_event_line() {
        assert_eq!(SseFrame::data("[DONE]").to_wire(), "data: [DONE]\n\n");
    }

    #[test]
    fn event_frame_includes_event_line() {
        let frame = SseFrame {
            event: Some("message_stop".to_string()),
            data: "{}".to_string(),
        };
        assert_eq!(frame.to_wire(), "event: message_stop\ndata: {}\n\n");
    }
}
