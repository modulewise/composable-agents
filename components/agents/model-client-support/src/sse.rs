//! Server-sent events, parsed as the WHATWG HTML Standard defines them in
//! "Parsing an event stream" (section 9.2.5) and
//! "Interpreting an event stream" (section 9.2.6).

use std::collections::VecDeque;

use wit_bindgen::rt::async_support::{StreamReader, StreamResult};

/// A dispatched event: its `event` field, if any, and its `data` fields
/// joined by line feeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub event_type: Option<String>,
    pub data: String,
}

impl Event {
    /// Whether the data is `[DONE]`, which some APIs send after the last event.
    pub fn is_done(&self) -> bool {
        self.data == "[DONE]"
    }
}

/// Parses an event stream incrementally, yielding each event at the blank line
/// that ends it.
#[derive(Default)]
pub struct Parser {
    // The bytes of the line being read.
    line: Vec<u8>,
    // Whether the last byte was a CR, so that an LF right after it completes
    // a CRLF rather than ending another line.
    after_cr: bool,
    // Whether the first line has been read. Only the first line can start
    // with a byte order mark.
    started: bool,
    data: String,
    event_type: String,
}

impl Parser {
    /// The events that `bytes` complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for &byte in bytes {
            // "a U+000D CARRIAGE RETURN U+000A LINE FEED (CRLF) character pair,
            // a single U+000A LINE FEED (LF) character not preceded by a
            // U+000D CARRIAGE RETURN (CR) character, and a single U+000D
            // CARRIAGE RETURN (CR) character not followed by a U+000A LINE
            // FEED (LF) character being the ways in which a line can end."
            if std::mem::take(&mut self.after_cr) && byte == b'\n' {
                continue;
            }
            match byte {
                b'\r' => {
                    self.after_cr = true;
                    self.end_line(&mut events);
                }
                b'\n' => self.end_line(&mut events),
                _ => self.line.push(byte),
            }
        }
        events
    }

    fn end_line(&mut self, events: &mut Vec<Event>) {
        // CR and LF never occur inside a UTF-8 sequence, so each line is
        // decoded on its own. Invalid bytes become U+FFFD, as in the spec's
        // UTF-8 decode.
        let bytes = std::mem::take(&mut self.line);
        let decoded = String::from_utf8_lossy(&bytes);
        // "The UTF-8 decode algorithm strips one leading UTF-8 Byte Order Mark
        // (BOM), if any."
        let line = if std::mem::replace(&mut self.started, true) {
            &decoded[..]
        } else {
            decoded.strip_prefix('\u{feff}').unwrap_or(&decoded)
        };
        self.process(line, events);
    }

    fn process(&mut self, line: &str, events: &mut Vec<Event>) {
        // "If the line is empty (a blank line) Dispatch the event".
        if line.is_empty() {
            self.dispatch(events);
            return;
        }
        // "If the line starts with a U+003A COLON character (:) Ignore the
        // line."
        if line.starts_with(':') {
            return;
        }
        // The field name is everything before the first colon, and the value
        // is everything after it, minus one leading space. A line without a
        // colon is a field with an empty value.
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.event_type = value.to_string(),
            "data" => {
                self.data.push_str(value);
                self.data.push('\n');
            }
            // `id` and `retry` (for reconnecting) and unknown fields are ignored.
            _ => {}
        }
    }

    fn dispatch(&mut self, events: &mut Vec<Event>) {
        // Both buffers are emptied whether or not an event is dispatched.
        let mut data = std::mem::take(&mut self.data);
        let event_type = std::mem::take(&mut self.event_type);
        // "If the data buffer is an empty string, set the data buffer and the
        // event type buffer to the empty string and return."
        if data.is_empty() {
            return;
        }
        // "If the data buffer's last character is a U+000A LINE FEED (LF)
        // character, then remove the last character from the data buffer."
        if data.ends_with('\n') {
            data.pop();
        }
        events.push(Event {
            event_type: (!event_type.is_empty()).then_some(event_type),
            data,
        });
    }
}

/// The events of a response body, read as they arrive. Dropping it drops the
/// body, which closes the connection.
pub struct Events {
    body: StreamReader<u8>,
    parser: Parser,
    pending: VecDeque<Event>,
    ended: bool,
}

impl Events {
    pub fn new(body: StreamReader<u8>) -> Self {
        Events {
            body,
            parser: Parser::default(),
            pending: VecDeque::new(),
            ended: false,
        }
    }

    /// The next event, or `None` once the body has ended. "If the file ends in
    /// the middle of an event, before the final empty line, the incomplete
    /// event is not dispatched."
    pub async fn next(&mut self) -> Option<Event> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(event);
            }
            if self.ended {
                return None;
            }
            let (result, chunk) = self.body.read(Vec::with_capacity(8192)).await;
            self.pending.extend(self.parser.feed(&chunk));
            if matches!(result, StreamResult::Dropped) {
                self.ended = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(event_type: Option<&str>, data: &str) -> Event {
        Event {
            event_type: event_type.map(str::to_string),
            data: data.to_string(),
        }
    }

    // Feeds `stream` in chunks of `size` bytes.
    fn parse_in_chunks(stream: &str, size: usize) -> Vec<Event> {
        let mut parser = Parser::default();
        stream
            .as_bytes()
            .chunks(size)
            .flat_map(|chunk| parser.feed(chunk))
            .collect()
    }

    #[test]
    fn an_event_is_dispatched_at_the_blank_line_that_ends_it() {
        let stream = "event: message_start\ndata: {\"type\":\"message_start\"}\n\ndata: second\n\n";
        let expected = vec![
            event(Some("message_start"), r#"{"type":"message_start"}"#),
            event(None, "second"),
        ];
        assert_eq!(Parser::default().feed(stream.as_bytes()), expected);
        // However the bytes are split into chunks.
        for size in 1..stream.len() {
            assert_eq!(parse_in_chunks(stream, size), expected, "chunks of {size}");
        }
    }

    #[test]
    fn data_fields_join_with_line_feeds() {
        assert_eq!(
            Parser::default().feed(b"data: {\ndata: \"a\": 1\ndata: }\n\n"),
            vec![event(None, "{\n\"a\": 1\n}")]
        );
        // An empty data field adds an empty line.
        assert_eq!(
            Parser::default().feed(b"data\ndata: x\n\n"),
            vec![event(None, "\nx")]
        );
    }

    #[test]
    fn lines_end_at_crlf_lf_or_cr() {
        let expected = vec![event(Some("a"), "1"), event(Some("b"), "2")];
        for stream in [
            "event: a\r\ndata: 1\r\n\r\nevent: b\r\ndata: 2\r\n\r\n",
            "event: a\ndata: 1\n\nevent: b\ndata: 2\n\n",
            "event: a\rdata: 1\r\revent: b\rdata: 2\r\r",
            "event: a\r\ndata: 1\n\revent: b\rdata: 2\n\r\n",
        ] {
            assert_eq!(
                Parser::default().feed(stream.as_bytes()),
                expected,
                "{stream:?}"
            );
            // A CRLF split across chunks is one line end.
            assert_eq!(parse_in_chunks(stream, 1), expected, "{stream:?}");
        }
    }

    #[test]
    fn a_value_loses_one_leading_space() {
        assert_eq!(
            Parser::default().feed(b"data:no space\n\ndata:  two spaces\n\n"),
            vec![event(None, "no space"), event(None, " two spaces")]
        );
        // The value is everything after the first colon.
        assert_eq!(
            Parser::default().feed(b"data: a: b\n\n"),
            vec![event(None, "a: b")]
        );
    }

    #[test]
    fn comments_and_other_fields_are_ignored() {
        assert_eq!(
            Parser::default().feed(b": ping\nid: 7\nretry: 100\nother: x\ndata: kept\n\n"),
            vec![event(None, "kept")]
        );
    }

    #[test]
    fn an_event_without_data_is_not_dispatched_and_its_type_is_cleared() {
        assert_eq!(
            Parser::default().feed(b"event: lost\n\ndata: next\n\n"),
            vec![event(None, "next")]
        );
    }

    #[test]
    fn field_names_are_case_sensitive() {
        assert_eq!(
            Parser::default().feed(b"Data: ignored\nEVENT: ignored\ndata: kept\n\n"),
            vec![event(None, "kept")]
        );
    }

    #[test]
    fn a_leading_byte_order_mark_is_stripped_once() {
        assert_eq!(
            Parser::default().feed("\u{feff}data: first\n\n".as_bytes()),
            vec![event(None, "first")]
        );
        // A BOM is stripped only at the start of the stream. A later one is
        // part of the field name, so the field isn't `data`.
        assert_eq!(
            Parser::default().feed("data: first\n\n\u{feff}data: second\n\n".as_bytes()),
            vec![event(None, "first")]
        );
        // A BOM split across chunks is still stripped.
        assert_eq!(
            parse_in_chunks("\u{feff}data: first\n\n", 1),
            vec![event(None, "first")]
        );
    }

    #[test]
    fn invalid_utf8_becomes_the_replacement_character() {
        assert_eq!(
            Parser::default().feed(b"data: a\xffb\n\n"),
            vec![event(None, "a\u{fffd}b")]
        );
    }

    #[test]
    fn an_event_the_stream_ends_in_the_middle_of_is_not_dispatched() {
        assert_eq!(
            Parser::default().feed(b"data: complete\n\ndata: cut off\n"),
            vec![event(None, "complete")]
        );
    }

    #[test]
    fn done_is_recognized() {
        assert!(event(Some("done"), "[DONE]").is_done());
        assert!(!event(None, "{}").is_done());
    }
}
