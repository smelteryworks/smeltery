//! Reading a few text fields (`_token`, `_method`) from the start of a `multipart/form-data`
//! body without holding the whole body: the bytes read are handed on, followed by the rest of
//! the stream, so the handler's extractor still sees every byte (D-130).

use axum::body::Body;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt as _;

/// The `boundary` parameter of a `multipart/form-data` content type.
pub(crate) fn boundary(content_type: &str) -> Option<String> {
    let mut parts = content_type.split(';');
    if !parts
        .next()?
        .trim()
        .eq_ignore_ascii_case("multipart/form-data")
    {
        return None;
    }
    parts.find_map(|p| {
        let (k, v) = p.trim().split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("boundary")
            .then(|| v.trim().trim_matches('"').to_owned())
            .filter(|b| !b.is_empty())
    })
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// The text value of `part` (one multipart part, between two delimiters) when it is the text
/// field `name` (a file part never matches).
fn part_value(part: &[u8], name: &str) -> Option<String> {
    let split = find(part, b"\r\n\r\n", 0)?;
    let head = String::from_utf8_lossy(part.get(..split)?).to_ascii_lowercase();
    let wanted = format!("name=\"{name}\"");
    let is_field = head.lines().any(|l| {
        l.starts_with("content-disposition:") && l.contains(&wanted) && !l.contains("filename=")
    });
    if !is_field {
        return None;
    }
    let value = part.get(split + 4..)?;
    let value = value.strip_suffix(b"\r\n").unwrap_or(value);
    String::from_utf8(value.to_vec()).ok()
}

/// Finds complete parts in a growing buffer; each byte is searched about once, so a body
/// arriving in many chunks costs linear time.
struct Scanner {
    delimiter: Vec<u8>,
    /// Where the delimiter opening the current (incomplete) part starts.
    start: Option<usize>,
    /// Where the search for the next delimiter resumes.
    searched: usize,
}

impl Scanner {
    fn new(boundary: &str) -> Self {
        Self {
            delimiter: format!("--{boundary}").into_bytes(),
            start: None,
            searched: 0,
        }
    }

    /// Look at the parts completed since the last call; record the wanted fields found.
    fn scan(&mut self, buf: &[u8], wanted: &[&str], found: &mut Vec<(String, String)>) {
        let d = self.delimiter.len();
        loop {
            let from = match self.start {
                Some(start) => self.searched.max(start + d),
                None => self.searched,
            };
            let Some(next) = find(buf, &self.delimiter, from) else {
                // A delimiter may straddle the end of the buffer: resume a little before it.
                self.searched = buf.len().saturating_sub(d).max(from.min(buf.len()));
                return;
            };
            if let Some(start) = self.start
                && let Some(part) = buf.get(start + d..next)
            {
                for name in wanted {
                    if !found.iter().any(|(k, _)| k == name)
                        && let Some(value) = part_value(part, name)
                    {
                        found.push(((*name).to_owned(), value));
                    }
                }
            }
            self.start = Some(next);
            self.searched = next + d;
        }
    }
}

/// Read `body` until every `wanted` text field is found, the body ends, or `limit` bytes are
/// buffered; returns the fields found and a body that replays every byte.
pub(crate) async fn peek_fields(
    body: Body,
    boundary: &str,
    wanted: &[&str],
    limit: usize,
) -> (Body, Vec<(String, String)>) {
    let mut stream = body.into_data_stream();
    let mut buf = BytesMut::new();
    let mut scanner = Scanner::new(boundary);
    let mut found = Vec::new();
    let mut failed = None;
    let mut ended = false;
    while buf.len() < limit && found.len() < wanted.len() {
        match stream.next().await {
            Some(Ok(chunk)) => {
                buf.extend_from_slice(&chunk);
                scanner.scan(&buf, wanted, &mut found);
            }
            Some(Err(e)) => {
                failed = Some(e);
                break;
            }
            None => {
                ended = true;
                break;
            }
        }
    }
    let head: Bytes = buf.freeze();
    if ended {
        return (Body::from(head), found);
    }
    let head = futures_util::stream::once(async move { Ok::<Bytes, axum::Error>(head) });
    let body = match failed {
        // The error goes to the handler's extractor, which reports it.
        Some(e) => Body::from_stream(head.chain(futures_util::stream::once(async move {
            Err::<Bytes, axum::Error>(e)
        }))),
        None => Body::from_stream(head.chain(stream)),
    };
    (body, found)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    const BODY: &[u8] = b"--XyZ\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nHello\r\n\
--XyZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"_token\"\r\n\r\nbinary\r\n\
--XyZ\r\nContent-Disposition: form-data; name=\"_token\"\r\n\r\nabc123\r\n--XyZ--\r\n";

    fn scan_all(buf: &[u8], wanted: &[&str], step: usize) -> Vec<(String, String)> {
        let mut scanner = Scanner::new("XyZ");
        let mut found = Vec::new();
        let mut end = 0;
        while end < buf.len() {
            end = (end + step).min(buf.len());
            scanner.scan(&buf[..end], wanted, &mut found);
        }
        found
    }

    #[test]
    fn finds_text_fields_in_any_chunking() {
        for step in [1, 2, 3, 7, 50, BODY.len()] {
            let found = scan_all(BODY, &["_token", "title", "missing"], step);
            assert_eq!(
                found,
                vec![
                    ("title".to_owned(), "Hello".to_owned()),
                    ("_token".to_owned(), "abc123".to_owned())
                ],
                "step {step}"
            );
        }
    }

    #[test]
    fn boundaries() {
        assert_eq!(
            boundary("multipart/form-data; boundary=\"XyZ\"").as_deref(),
            Some("XyZ")
        );
        assert_eq!(boundary("text/plain; boundary=x"), None);
        assert_eq!(boundary("multipart/form-data"), None);
    }

    #[tokio::test]
    async fn peeking_replays_every_byte() {
        let chunked = || {
            let chunks: Vec<Result<Bytes, std::io::Error>> = BODY
                .chunks(5)
                .map(|c| Ok(Bytes::copy_from_slice(c)))
                .collect();
            Body::from_stream(futures_util::stream::iter(chunks))
        };
        let body = chunked();
        let (body, found) = peek_fields(body, "XyZ", &["title"], 1024).await;
        assert_eq!(found, vec![("title".to_owned(), "Hello".to_owned())]);
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(&bytes[..], BODY);

        // Past the limit the search stops, and the body is still whole.
        let (body, found) = peek_fields(chunked(), "XyZ", &["_token"], 10).await;
        assert!(found.is_empty());
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(&bytes[..], BODY);
    }
}
