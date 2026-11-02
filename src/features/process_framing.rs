//! Length-header framing: `Header: value\r\n ... \r\n\r\n` followed by exactly
//! `Content-Length` arbitrary bytes. The decoder takes bytes in any chunking, so a
//! read that ends in the middle of a header, in the middle of a body, or holds
//! several messages at once is handled the same as a read of one message.
use std::fmt;

/// The largest message accepted. A server that sends more is treated as broken.
pub const DEFAULT_MAX_MESSAGE: usize = 32 * 1024 * 1024;
/// Headers are a few short lines; anything longer is not a header.
const MAX_HEADER: usize = 8 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// A header line without a colon, or not valid text.
    BadHeader(String),
    /// A header block that never ends or is absurdly long.
    HeaderTooLong,
    MissingLength,
    BadLength(String),
    TooLarge {
        length: usize,
        limit: usize,
    },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadHeader(line) => write!(f, "malformed header line {line:?}"),
            Self::HeaderTooLong => f.write_str("the message header is too long"),
            Self::MissingLength => f.write_str("the message has no Content-Length"),
            Self::BadLength(value) => write!(f, "invalid Content-Length {value:?}"),
            Self::TooLarge { length, limit } => {
                write!(
                    f,
                    "a message of {length} bytes exceeds the limit of {limit}"
                )
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// Wraps a body in a frame.
pub fn encode(body: &[u8]) -> Vec<u8> {
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(body);
    frame
}

#[derive(Debug)]
pub struct Decoder {
    buffer: Vec<u8>,
    /// The body length once its header has been read.
    expecting: Option<usize>,
    limit: usize,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_MESSAGE)
    }
}

impl Decoder {
    pub fn new(limit: usize) -> Self {
        Self {
            buffer: Vec::new(),
            expecting: None,
            limit,
        }
    }

    /// Adds `bytes` and returns every message they complete. After an error the
    /// stream cannot be resynchronised, so the caller must drop the connection.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        self.buffer.extend_from_slice(bytes);
        let mut messages = Vec::new();
        loop {
            let length = match self.expecting {
                Some(length) => length,
                None => match self.header()? {
                    Some(length) => {
                        self.expecting = Some(length);
                        length
                    }
                    None => return Ok(messages),
                },
            };
            if self.buffer.len() < length {
                return Ok(messages);
            }
            let rest = self.buffer.split_off(length);
            messages.push(std::mem::replace(&mut self.buffer, rest));
            self.expecting = None;
        }
    }

    /// Bytes held that do not yet make a whole message.
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    /// A complete stream has neither a partial header nor an unfinished body.
    pub fn is_complete(&self) -> bool {
        self.buffer.is_empty() && self.expecting.is_none()
    }

    /// Consumes a complete header block from the front of the buffer and returns
    /// its Content-Length, or `None` when the block is not complete yet.
    fn header(&mut self) -> Result<Option<usize>, FrameError> {
        let Some(end) = self.buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
            if self.buffer.len() > MAX_HEADER {
                return Err(FrameError::HeaderTooLong);
            }
            return Ok(None);
        };
        if end > MAX_HEADER {
            return Err(FrameError::HeaderTooLong);
        }
        let block = std::str::from_utf8(&self.buffer[..end])
            .map_err(|_| FrameError::BadHeader("(not text)".into()))?;
        let mut length = None;
        for line in block.split("\r\n") {
            let Some((name, value)) = line.split_once(':') else {
                return Err(FrameError::BadHeader(line.to_owned()));
            };
            if name.trim().eq_ignore_ascii_case("content-length") {
                let value = value.trim();
                let parsed: usize = value
                    .parse()
                    .map_err(|_| FrameError::BadLength(value.to_owned()))?;
                if length.replace(parsed).is_some() {
                    return Err(FrameError::BadLength("repeated".into()));
                }
            }
            // Other headers, such as Content-Type, carry nothing we need.
        }
        let length = length.ok_or(FrameError::MissingLength)?;
        if length > self.limit {
            return Err(FrameError::TooLarge {
                length,
                limit: self.limit,
            });
        }
        self.buffer.drain(..end + 4);
        Ok(Some(length))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(body: &str) -> Vec<u8> {
        encode(body.as_bytes())
    }

    fn all(decoder: &mut Decoder, bytes: &[u8]) -> Vec<String> {
        decoder
            .push(bytes)
            .unwrap()
            .into_iter()
            .map(|m| String::from_utf8(m).unwrap())
            .collect()
    }

    #[test]
    fn one_message_in_one_read() {
        let mut decoder = Decoder::default();
        assert_eq!(all(&mut decoder, &frame("{\"a\":1}")), ["{\"a\":1}"]);
        assert_eq!(decoder.pending(), 0);
    }

    #[test]
    fn the_length_counts_bytes_not_characters() {
        let body = "{\"t\":\"中文😀é\"}";
        assert_ne!(body.len(), body.chars().count());
        let mut decoder = Decoder::default();
        assert_eq!(all(&mut decoder, &frame(body)), [body]);
    }

    #[test]
    fn several_messages_in_one_read_come_out_in_order() {
        let mut bytes = frame("{\"n\":1}");
        bytes.extend(frame("{\"n\":2}"));
        bytes.extend(frame("{\"n\":3}"));
        assert_eq!(
            all(&mut Decoder::default(), &bytes),
            ["{\"n\":1}", "{\"n\":2}", "{\"n\":3}"]
        );
    }

    #[test]
    fn every_split_point_gives_the_same_messages() {
        let mut bytes = frame("{\"first\":\"中\"}");
        bytes.extend(frame("{\"second\":true}"));
        let expected = ["{\"first\":\"中\"}", "{\"second\":true}"];
        for cut in 0..=bytes.len() {
            let mut decoder = Decoder::default();
            let mut got = all(&mut decoder, &bytes[..cut]);
            got.extend(all(&mut decoder, &bytes[cut..]));
            assert_eq!(got, expected, "split at {cut}");
        }
        // One byte at a time, the worst case for a pipe.
        let mut decoder = Decoder::default();
        let mut got = Vec::new();
        for byte in &bytes {
            got.extend(all(&mut decoder, std::slice::from_ref(byte)));
        }
        assert_eq!(got, expected);
        assert_eq!(decoder.pending(), 0);
    }

    #[test]
    fn extra_headers_and_any_case_are_accepted() {
        let body = "{}";
        let bytes = format!(
            "content-type: application/vscode-jsonrpc; charset=utf-8\r\nCONTENT-LENGTH:   {}  \r\n\r\n{body}",
            body.len()
        );
        assert_eq!(all(&mut Decoder::default(), bytes.as_bytes()), ["{}"]);
    }

    #[test]
    fn an_empty_body_is_a_message() {
        assert_eq!(
            all(&mut Decoder::default(), b"Content-Length: 0\r\n\r\n"),
            [""]
        );
    }

    #[test]
    fn broken_headers_are_errors_not_hangs() {
        let cases: [(&[u8], FrameError); 6] = [
            (
                b"Content-Length: abc\r\n\r\n",
                FrameError::BadLength("abc".into()),
            ),
            (
                b"Content-Length: -3\r\n\r\n",
                FrameError::BadLength("-3".into()),
            ),
            (
                b"Content-Length: 1\r\nContent-Length: 1\r\n\r\nx",
                FrameError::BadLength("repeated".into()),
            ),
            (b"Content-Type: x\r\n\r\n{}", FrameError::MissingLength),
            (
                b"garbage without a colon\r\n\r\n",
                FrameError::BadHeader("garbage without a colon".into()),
            ),
            (
                b"Content-Length: 99999999999\r\n\r\n",
                FrameError::TooLarge {
                    length: 99_999_999_999,
                    limit: DEFAULT_MAX_MESSAGE,
                },
            ),
        ];
        for (bytes, expected) in cases {
            assert_eq!(Decoder::default().push(bytes).unwrap_err(), expected);
        }
        // A header block that never ends is cut off rather than buffered forever.
        let endless = vec![b'x'; MAX_HEADER + 1];
        assert_eq!(
            Decoder::default().push(&endless).unwrap_err(),
            FrameError::HeaderTooLong
        );
        // Not text.
        assert!(matches!(
            Decoder::default().push(b"\xff\xfe: 1\r\n\r\n"),
            Err(FrameError::BadHeader(_))
        ));
    }

    #[test]
    fn the_limit_is_checked_before_any_body_is_read() {
        let mut decoder = Decoder::new(10);
        assert!(
            decoder
                .push(b"Content-Length: 10\r\n\r\n0123456789")
                .is_ok()
        );
        let error = Decoder::new(10)
            .push(b"Content-Length: 11\r\n\r\n")
            .unwrap_err();
        assert_eq!(
            error,
            FrameError::TooLarge {
                length: 11,
                limit: 10
            }
        );
        assert!(error.to_string().contains("exceeds the limit"));
    }

    #[test]
    fn encode_round_trips_through_the_decoder() {
        for body in ["", "{}", "{\"k\":\"v\"}", "中文"] {
            let mut decoder = Decoder::default();
            assert_eq!(all(&mut decoder, &encode(body.as_bytes())), [body]);
        }
        assert_eq!(encode(b"abc"), b"Content-Length: 3\r\n\r\nabc");
    }
}
