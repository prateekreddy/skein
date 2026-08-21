//! The smallest HTTP the warden can be spoken to in.
//!
//! **Why not axum.** skein's own rule is "compose, don't reinvent — standard wheels only", and it is
//! the right rule for skein. This component is the exception and the reason is what it is *for*: the
//! warden exists to be the thing a compromised skein has to get past, so its dependency list is part
//! of its argument. `axum` brings tokio, hyper, tower and their transitive tree into the one process
//! on the host that runs privileged commands. What is actually needed is one method, one path, a
//! `Content-Length` body under a cap, from one client, on loopback — under two hundred lines that a
//! person can read in full. That trade is worth making exactly once, here, and the crate note says
//! so where anyone adding a dependency will see it.
//!
//! **The parser is a strict subset, and each restriction removes a class of bug rather than a
//! feature nobody wanted:**
//!
//! * **One request per connection**, and the connection is closed after the reply. No keep-alive,
//!   no pipelining — which is what makes request smuggling impossible by construction rather than by
//!   careful agreement between two length rules.
//! * **`Content-Length` only.** `Transfer-Encoding` is refused outright. The two disagreeing is the
//!   other half of smuggling, and a warden has no use for chunked bodies.
//! * **A cap on the request line, on the headers, and on the body**, all read through a
//!   [`std::io::Take`], so a client that promises a gigabyte cannot make the warden allocate one.
//! * **A duplicate `Content-Length` is refused**, rather than the first or last winning.
//!
//! Nothing here authenticates. That is deliberate and it is not an omission to be fixed here:
//! connecting is not authenticating (§9.4), and the warden's answer to "who is asking" is not a
//! header at all — it is that the approval is confirmed by a human on the host (§8.1).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};

/// Longest request line — `POST /v1/destroy HTTP/1.1`, with room to spare.
const MAX_LINE: u64 = 8 * 1024;
/// Longest block of headers.
const MAX_HEADERS: u64 = 16 * 1024;
/// Longest body. An operation request is a few hundred bytes; this is three orders of margin.
pub const MAX_BODY: usize = 256 * 1024;

/// One request, already bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// Path only — a query string is stripped and ignored. The warden takes its arguments in the
    /// body, where they are one parse rather than two.
    pub path: String,
    pub body: Vec<u8>,
}

/// What to send back. `code` and a JSON body; the warden speaks nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub code: u16,
    pub body: String,
}

impl Response {
    pub fn json(code: u16, body: impl Into<String>) -> Response {
        Response {
            code,
            body: body.into(),
        }
    }

    /// An error, as the one shape every failing path answers in.
    pub fn fault(code: u16, why: &str) -> Response {
        Response::json(code, serde_json::json!({ "error": why }).to_string())
    }

    fn reason(&self) -> &'static str {
        match self.code {
            200 => "OK",
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            409 => "Conflict",
            413 => "Payload Too Large",
            500 => "Internal Server Error",
            _ => "Status",
        }
    }

    /// Write the reply and say the connection is ending, because it is.
    pub fn write_to(&self, out: &mut impl Write) -> std::io::Result<()> {
        write!(
            out,
            "HTTP/1.1 {} {}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n{}",
            self.code,
            self.reason(),
            self.body.len(),
            self.body
        )?;
        out.flush()
    }
}

/// Read exactly one request, or say why it is not one.
///
/// The `Err` is a [`Response`] rather than a string: every way this can fail is a reply the caller
/// has to send, and returning the reply itself is what stops a new failure being added without a
/// status code chosen for it.
pub fn read_request(stream: &mut impl Read) -> Result<Request, Response> {
    let mut reader = BufReader::new(stream);

    let line = read_line(&mut reader, MAX_LINE)
        .ok_or_else(|| Response::fault(400, "the request line was unreadable or too long"))?;
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(Response::fault(
            400,
            "the request line is not `METHOD TARGET VERSION`",
        ));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(Response::fault(400, "this warden speaks HTTP/1.1 only"));
    }

    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    let mut seen = 0u64;
    loop {
        let header = read_line(&mut reader, MAX_LINE)
            .ok_or_else(|| Response::fault(400, "a header was unreadable or too long"))?;
        if header.is_empty() {
            break;
        }
        seen += header.len() as u64 + 2;
        if seen > MAX_HEADERS {
            return Err(Response::fault(431, "too many headers"));
        }
        let Some((name, value)) = header.split_once(':') else {
            return Err(Response::fault(400, "a header has no colon in it"));
        };
        let name = name.trim().to_ascii_lowercase();
        // Refused rather than merged. Two `Content-Length`s that disagree is one half of request
        // smuggling, and "the first one wins" is a decision no warden should be making quietly.
        if headers
            .insert(name.clone(), value.trim().to_string())
            .is_some()
        {
            return Err(Response::fault(
                400,
                &format!("the header `{name}` appears twice"),
            ));
        }
    }

    // The other half. There is no use for chunked here, and accepting it means owning the
    // disagreement between two ways of saying how long a body is.
    if headers.contains_key("transfer-encoding") {
        return Err(Response::fault(
            400,
            "this warden takes a Content-Length body and nothing else",
        ));
    }

    let length: usize = match headers.get("content-length") {
        None => 0,
        Some(raw) => raw
            .parse()
            .map_err(|_| Response::fault(400, "Content-Length is not a number"))?,
    };
    if length > MAX_BODY {
        return Err(Response::fault(413, "the request body is too large"));
    }
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|_| Response::fault(400, "the body was shorter than Content-Length said"))?;

    Ok(Request {
        method: method.to_string(),
        path: target.split(['?', '#']).next().unwrap_or("").to_string(),
        body,
    })
}

/// One CRLF-terminated line, up to `cap` bytes, with the terminator removed.
///
/// `None` for "could not read one", which includes hitting the cap — a caller cannot tell those
/// apart and does not need to: both are the same refusal.
fn read_line(reader: &mut impl BufRead, cap: u64) -> Option<String> {
    let mut raw = Vec::new();
    let read = reader.take(cap).read_until(b'\n', &mut raw).ok()?;
    if read == 0 || raw.last() != Some(&b'\n') {
        return None;
    }
    raw.pop();
    if raw.last() == Some(&b'\r') {
        raw.pop();
    }
    String::from_utf8(raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Result<Request, Response> {
        read_request(&mut std::io::Cursor::new(raw.as_bytes().to_vec()))
    }

    #[test]
    fn an_ordinary_request_parses() {
        let got =
            parse("POST /v1/create?ignored=1 HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\n{}")
                .unwrap();
        assert_eq!(got.method, "POST");
        assert_eq!(
            got.path, "/v1/create",
            "the query string is not part of the path"
        );
        assert_eq!(got.body, b"{}");

        let no_body = parse("GET /v1/fleet HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert_eq!(no_body.method, "GET");
        assert!(no_body.body.is_empty());
    }

    /// The two ways of saying how long a body is must never both be in play, and neither must be
    /// said twice. Both are request smuggling, and both are refused rather than reconciled.
    #[test]
    fn the_shapes_that_make_smuggling_possible_are_refused() {
        let chunked =
            parse("POST /v1/create HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap_err();
        assert_eq!(chunked.code, 400);
        assert!(chunked
            .body
            .contains("Content-Length body and nothing else"));

        let twice =
            parse("POST /v1/create HTTP/1.1\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\n{}")
                .unwrap_err();
        assert_eq!(twice.code, 400);
        assert!(
            twice.body.contains("appears twice"),
            "a duplicate length must be refused, not resolved: {}",
            twice.body
        );
    }

    /// A promise the warden must not act on by allocating.
    #[test]
    fn a_body_larger_than_the_cap_is_refused_rather_than_reserved() {
        let huge = format!(
            "POST /v1/create HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        let refused = parse(&huge).unwrap_err();
        assert_eq!(refused.code, 413);

        // And a body that does not arrive is a failure rather than a short read treated as valid.
        let short = parse("POST /v1/create HTTP/1.1\r\nContent-Length: 10\r\n\r\n{}").unwrap_err();
        assert_eq!(short.code, 400);
        assert!(short.body.contains("shorter than Content-Length"));
    }

    #[test]
    fn a_request_line_that_is_not_one_is_refused() {
        assert_eq!(parse("GET\r\n\r\n").unwrap_err().code, 400);
        assert_eq!(parse("GET / HTTP/9\r\n\r\n").unwrap_err().code, 400);
        assert_eq!(
            parse("GET / HTTP/1.1\r\nno-colon\r\n\r\n")
                .unwrap_err()
                .code,
            400
        );
        // An unterminated request line is not a request, however long it goes on.
        assert_eq!(parse("GET / HTTP/1.1").unwrap_err().code, 400);
    }

    /// Every reply says the connection is over, because it is: one request per connection is what
    /// makes the parser's simplicity safe rather than merely short.
    #[test]
    fn every_reply_closes_the_connection() {
        let mut out = Vec::new();
        Response::json(200, "{\"ok\":true}")
            .write_to(&mut out)
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.contains("Content-Length: 11\r\n"));
        assert!(text.ends_with("\r\n\r\n{\"ok\":true}"));
    }
}
