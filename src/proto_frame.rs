use bytes::{Buf, BytesMut};
use nom::{
    branch::alt,
    bytes::streaming::{is_not, tag, take_until},
    character::streaming::{char, crlf, digit1, space0},
    combinator::{map, map_res, opt},
    multi::length_data,
    sequence::{delimited, terminated, tuple},
    IResult,
};
use std::io::Write;
use std::str;

#[derive(Debug)]
#[allow(dead_code)]
pub enum FrameError {
    MissingHeader,
    InvalidLength,
    Utf8(std::str::Utf8Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingHeader => write!(f, "missing required `Content-Length` header"),
            Self::InvalidLength => write!(f, "unable to parse content length"),
            Self::Utf8(e) => write!(f, "frame contains invalid UTF8: {}", e),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Utf8(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::str::Utf8Error> for FrameError {
    fn from(error: std::str::Utf8Error) -> Self {
        Self::Utf8(error)
    }
}

pub struct FrameDecoder {
    buffer: BytesMut,
    expected_length: Option<usize>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self {
            buffer: BytesMut::new(),
            expected_length: None,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, FrameError> {
        self.buffer.extend_from_slice(bytes);
        let mut messages = Vec::new();

        loop {
            if let Some(len) = self.expected_length {
                if self.buffer.len() >= len {
                    let payload = self.buffer.split_to(len);
                    self.expected_length = None;
                    let msg = str::from_utf8(&payload)?.to_string();
                    if !msg.is_empty() {
                        messages.push(msg);
                    }
                } else {
                    break;
                }
            } else {
                match parse_message(&self.buffer) {
                    Ok((remaining, message)) => {
                        let msg = str::from_utf8(message)?.to_string();
                        let consumed = self.buffer.len() - remaining.len();
                        self.buffer.advance(consumed);
                        self.expected_length = None;
                        if !msg.is_empty() {
                            messages.push(msg);
                        }
                    }
                    Err(nom::Err::Incomplete(_)) => break,
                    Err(_) => {
                        if let Ok((_, pos)) = find_next_message(&self.buffer) {
                            self.buffer.advance(pos);
                        } else {
                            self.buffer.clear();
                        }
                        self.expected_length = None;
                    }
                }
            }
        }

        Ok(messages)
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

pub fn encode_frame(payload: &str) -> Vec<u8> {
    let len = payload.len();
    let mut result = Vec::with_capacity(21 + len);
    write!(&mut result, "Content-Length: {}\r\n\r\n{}", len, payload).unwrap();
    result
}

pub(crate) fn parse_message(input: &[u8]) -> IResult<&[u8], &[u8]> {
    let content_len = delimited(tag("Content-Length: "), digit1, crlf);

    let utf8 = alt((tag("utf-8"), tag("utf8")));
    let charset = tuple((char(';'), space0, tag("charset="), utf8));
    let content_type = tuple((tag("Content-Type: "), is_not(";\r"), opt(charset), crlf));

    let header = terminated(terminated(content_len, opt(content_type)), crlf);

    let header = map_res(header, str::from_utf8);
    let length = map_res(header, |s: &str| s.parse::<usize>());
    let mut message = length_data(length);

    message(input)
}

pub(crate) fn find_next_message(input: &[u8]) -> IResult<&[u8], usize> {
    map(take_until("Content-Length"), |s: &[u8]| s.len())(input)
}
