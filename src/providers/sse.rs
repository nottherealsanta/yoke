//! An incremental server-sent events parser.
//!
//! Bytes go in, complete events come out. A chunk may split anywhere, including
//! in the middle of a UTF-8 character or a CRLF pair.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct Parser {
    buffer: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl Parser {
    /// Feed one network chunk and return the events it completed.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Event> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(line) = self.take_line(false) {
            self.line(&line, &mut events);
        }
        events
    }

    /// Flush a final line that has no line ending, then a pending event.
    pub fn finish(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(line) = self.take_line(true) {
            self.line(&line, &mut events);
        }
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.line(&line, &mut events);
        }
        self.dispatch(&mut events);
        events
    }

    fn take_line(&mut self, at_eof: bool) -> Option<Vec<u8>> {
        for (index, byte) in self.buffer.iter().enumerate() {
            match byte {
                b'\n' => {
                    let line = self.buffer[..index].to_vec();
                    self.buffer.drain(..=index);
                    return Some(line);
                }
                b'\r' => {
                    if index + 1 == self.buffer.len() && !at_eof {
                        return None;
                    }
                    let end = if self.buffer.get(index + 1) == Some(&b'\n') {
                        index + 2
                    } else {
                        index + 1
                    };
                    let line = self.buffer[..index].to_vec();
                    self.buffer.drain(..end);
                    return Some(line);
                }
                _ => {}
            }
        }
        None
    }

    fn line(&mut self, line: &[u8], events: &mut Vec<Event>) {
        let line = String::from_utf8_lossy(line);
        if line.is_empty() {
            self.dispatch(events);
        } else if line.starts_with(':') {
            // A comment, often a keepalive.
        } else {
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line.as_ref(), ""),
            };
            match field {
                "data" => self.data.push(value.to_string()),
                "event" => self.event = Some(value.to_string()),
                _ => {}
            }
        }
    }

    fn dispatch(&mut self, events: &mut Vec<Event>) {
        let event = self.event.take();
        if self.data.is_empty() {
            return;
        }
        events.push(Event {
            event,
            data: std::mem::take(&mut self.data).join("\n"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Vec<Event> {
        let mut parser = Parser::default();
        let mut events = parser.feed(input.as_bytes());
        events.extend(parser.finish());
        events
    }

    fn event(data: &str) -> Event {
        Event {
            event: None,
            data: data.into(),
        }
    }

    #[test]
    fn data_and_event_fields() {
        assert_eq!(
            parse("data: hello\n\n"),
            [event("hello")],
            "a plain data event"
        );
        assert_eq!(
            parse("event: message\ndata: hello\n\n"),
            [Event {
                event: Some("message".into()),
                data: "hello".into(),
            }]
        );
        assert_eq!(parse("data:no space\n\n"), [event("no space")]);
    }

    #[test]
    fn multi_line_data_is_joined_with_newlines() {
        assert_eq!(
            parse("data: one\ndata: two\ndata: three\n\n"),
            [event("one\ntwo\nthree")]
        );
    }

    #[test]
    fn comments_and_unknown_fields_are_ignored() {
        assert_eq!(
            parse(": keepalive\nid: 7\ndata: hi\nretry: 100\n: again\n\n"),
            [event("hi")]
        );
        assert_eq!(parse(": only a comment\n\n"), []);
    }

    #[test]
    fn crlf_and_lone_cr_line_endings() {
        assert_eq!(parse("data: a\r\n\r\n"), [event("a")]);
        assert_eq!(parse("data: b\r\r"), [event("b")]);
        assert_eq!(parse("data: c\n\r\n"), [event("c")]);
    }

    #[test]
    fn chunks_may_split_everywhere() {
        let input = "event: message\r\ndata: héllo\r\ndata: wörld\r\n\r\ndata: [DONE]\r\n\r\n";
        let mut parser = Parser::default();
        let mut events = Vec::new();
        for byte in input.as_bytes() {
            events.extend(parser.feed(std::slice::from_ref(byte)));
        }
        events.extend(parser.finish());
        assert_eq!(
            events,
            [
                Event {
                    event: Some("message".into()),
                    data: "héllo\nwörld".into(),
                },
                event("[DONE]"),
            ]
        );
    }

    #[test]
    fn a_crlf_split_across_chunks_is_one_line_ending() {
        let mut parser = Parser::default();
        assert!(parser.feed(b"data: a\r").is_empty());
        assert_eq!(parser.feed(b"\n\r\n"), [event("a")]);
    }

    #[test]
    fn finish_flushes_an_unterminated_event() {
        assert_eq!(parse("data: last"), [event("last")]);
    }
}
