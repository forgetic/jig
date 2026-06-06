//! A minimal Server-Sent Events frame splitter.
//!
//! Splits a raw `text/event-stream` byte buffer into [`SseEvent`]s — one per
//! blank-line-delimited block — extracting the `event:` name (if any) and the
//! concatenated `data:` payload. This is the dialect-agnostic counterpart to
//! [`crate::render::SseFrame::to_wire`]: `to_wire` writes one block per frame,
//! and this reads them back.
//!
//! It implements only the slice of the SSE grammar the providers actually use
//! (the [WHATWG event-stream] rules for `event:`/`data:` fields and blank-line
//! dispatch), which is all the parsers in this module need:
//!
//! - A line `field: value` sets `field`. A leading space after the colon is
//!   stripped (one space only, per the spec).
//! - Multiple `data:` lines in one block are joined with `\n`.
//! - A blank line dispatches the accumulated block as one event.
//! - Lines beginning with `:` are comments and ignored.
//! - `\r\n` and `\n` are both accepted as line terminators, so a capture that
//!   preserved CRLF framing parses the same as a `\n`-only render.
//!
//! Events with an empty data buffer (e.g. a lone `event: ping` with no `data:`)
//! are still emitted: a caller that ignores `ping` does so by name, and dropping
//! empty-data events here would hide a malformed stream.
//!
//! [WHATWG event-stream]: https://html.spec.whatwg.org/multipage/server-sent-events.html

/// One parsed SSE event: its `event:` name (if the block carried one) and the
/// joined `data:` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field value, or `None` for a `data:`-only block.
    pub event: Option<String>,
    /// The `data:` payload — multiple `data:` lines joined with `\n`.
    pub data: String,
}

/// Strip a single optional leading space from a field value, per the SSE spec
/// ("if value starts with a U+0020 SPACE character, remove it").
fn strip_one_leading_space(value: &str) -> &str {
    value.strip_prefix(' ').unwrap_or(value)
}

/// Split a raw `text/event-stream` buffer into its [`SseEvent`]s, in order.
///
/// Lossy-decodes the bytes as UTF-8 first (provider streams are UTF-8; the
/// lossy step keeps a stray byte from aborting the whole parse). A trailing
/// block without a final blank line is still dispatched, so a stream truncated
/// at the last event is not silently dropped.
pub fn parse_sse(bytes: &[u8]) -> Vec<SseEvent> {
    let text = String::from_utf8_lossy(bytes);

    let mut events = Vec::new();
    let mut event: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();
    let mut have_field = false;

    let dispatch =
        |events: &mut Vec<SseEvent>, event: &mut Option<String>, data_lines: &mut Vec<String>| {
            events.push(SseEvent {
                event: event.take(),
                data: data_lines.join("\n"),
            });
            data_lines.clear();
        };

    for raw_line in text.split('\n') {
        // Accept CRLF as well as LF: split on '\n' leaves a trailing '\r'.
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

        if line.is_empty() {
            // Blank line: dispatch the block if it carried any field. A run of
            // blank lines between events does not emit empty events.
            if have_field {
                dispatch(&mut events, &mut event, &mut data_lines);
                have_field = false;
            }
            continue;
        }

        // Comment line (starts with ':') — ignored, and does not start a block.
        if let Some(rest) = line.strip_prefix(':') {
            let _ = rest;
            continue;
        }

        // `field: value` (or a bare `field` with no colon, value = "").
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, strip_one_leading_space(value)),
            None => (line, ""),
        };

        match field {
            "event" => {
                event = Some(value.to_string());
                have_field = true;
            }
            "data" => {
                data_lines.push(value.to_string());
                have_field = true;
            }
            // `id:` and `retry:` are valid SSE fields we don't need; ignore the
            // value but still treat the block as non-empty so a block carrying
            // only an id is dispatched rather than swallowed into the next one.
            "id" | "retry" => {
                have_field = true;
            }
            // Unknown field: ignore per spec.
            _ => {}
        }
    }

    // Dispatch a trailing block with no terminating blank line.
    if have_field {
        dispatch(&mut events, &mut event, &mut data_lines);
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_event_and_data_blocks() {
        let stream = "event: message_stop\ndata: {}\n\n";
        let events = parse_sse(stream.as_bytes());
        assert_eq!(
            events,
            vec![SseEvent {
                event: Some("message_stop".to_string()),
                data: "{}".to_string(),
            }]
        );
    }

    #[test]
    fn data_only_block_has_no_event() {
        let events = parse_sse(b"data: [DONE]\n\n");
        assert_eq!(
            events,
            vec![SseEvent {
                event: None,
                data: "[DONE]".to_string(),
            }]
        );
    }

    #[test]
    fn strips_exactly_one_leading_space() {
        // One space after the colon is stripped; a second space is data.
        let events = parse_sse(b"data:  two-leading\n\n");
        assert_eq!(events[0].data, " two-leading");
        // No space at all is fine too.
        let events = parse_sse(b"data:no-space\n\n");
        assert_eq!(events[0].data, "no-space");
    }

    #[test]
    fn joins_multiple_data_lines_with_newline() {
        let events = parse_sse(b"data: line1\ndata: line2\n\n");
        assert_eq!(events[0].data, "line1\nline2");
    }

    #[test]
    fn accepts_crlf_line_endings() {
        let events = parse_sse(b"event: ping\r\ndata: {}\r\n\r\n");
        assert_eq!(events[0].event.as_deref(), Some("ping"));
        assert_eq!(events[0].data, "{}");
    }

    #[test]
    fn ignores_comment_lines() {
        let events = parse_sse(b": this is a comment\nevent: x\ndata: 1\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("x"));
        assert_eq!(events[0].data, "1");
    }

    #[test]
    fn dispatches_trailing_block_without_final_blank_line() {
        let events = parse_sse(b"event: message_stop\ndata: {}");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("message_stop"));
    }

    #[test]
    fn emits_event_with_empty_data_buffer() {
        // A lone `event: ping` with no data still becomes an event so a caller
        // can ignore it by name rather than it vanishing.
        let events = parse_sse(b"event: ping\n\n");
        assert_eq!(
            events,
            vec![SseEvent {
                event: Some("ping".to_string()),
                data: String::new(),
            }]
        );
    }

    #[test]
    fn runs_of_blank_lines_do_not_emit_empty_events() {
        let events = parse_sse(b"\n\ndata: a\n\n\n\ndata: b\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "a");
        assert_eq!(events[1].data, "b");
    }

    #[test]
    fn parses_a_full_anthropic_style_sequence() {
        let stream = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\"}\n\n",
            "event: content_block_start\ndata: {\"index\":0}\n\n",
            "event: ping\ndata: {\"type\":\"ping\"}\n\n",
            "event: content_block_delta\ndata: {\"index\":0}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\"}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let events = parse_sse(stream.as_bytes());
        let names: Vec<&str> = events.iter().filter_map(|e| e.event.as_deref()).collect();
        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start",
                "ping",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
    }

    #[test]
    fn empty_input_yields_no_events() {
        assert!(parse_sse(b"").is_empty());
    }
}
