//! Client SDK for the RFC git-diff-ui endpoints.
//!
//! Three entry points used by both local and server modes:
//!
//! - [`stream_diff`] — posts a `DiffRequest` to `/api/projects/:id/diff` and
//!   returns an async stream of `DiffEventWire` items decoded from NDJSON.
//! - [`get_diff_sources`] — GETs `/api/projects/:id/diff/sources`.
//! - [`send_review`] — posts a `SendReviewRequest` to
//!   `/api/projects/:id/review/send`.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::Stream;
use zremote_protocol::project::{
    DiffError, DiffFile, DiffFileSummary, DiffRequest, DiffSourceOptions, SendReviewRequest,
    SendReviewResponse,
};

use crate::error::ApiError;

/// One decoded NDJSON line from the streaming diff endpoint. Matches the
/// agent-side `DiffEvent` and server-side `DiffStreamChunk` serde tags.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DiffEventWire {
    Started {
        files: Vec<DiffFileSummary>,
    },
    File {
        file_index: u32,
        file: DiffFile,
    },
    Finished {
        #[serde(default)]
        error: Option<DiffError>,
    },
}

/// `POST /api/projects/:project_id/diff` — returns a `Stream` yielding one
/// decoded `DiffEventWire` per NDJSON line. The underlying HTTP response is
/// kept alive by the stream; dropping the stream cancels the diff.
pub async fn stream_diff(
    base_url: &str,
    project_id: &str,
    request: &DiffRequest,
) -> Result<DiffEventStream, ApiError> {
    let url = format!(
        "{}/api/projects/{}/diff",
        base_url.trim_end_matches('/'),
        percent_encode(project_id)
    );
    let client = reqwest::Client::new();
    let response = client
        .post(url)
        .json(request)
        .send()
        .await
        .map_err(ApiError::Http)?;

    let status = response.status();
    if !status.is_success() {
        return Err(ApiError::from_response(response).await);
    }

    let bytes_stream = response.bytes_stream();
    Ok(DiffEventStream::new(Box::pin(bytes_stream)))
}

/// `GET /api/projects/:project_id/diff/sources`
pub async fn get_diff_sources(
    base_url: &str,
    project_id: &str,
    max_commits: Option<u32>,
) -> Result<DiffSourceOptions, ApiError> {
    use std::fmt::Write as _;
    let mut url = format!(
        "{}/api/projects/{}/diff/sources",
        base_url.trim_end_matches('/'),
        percent_encode(project_id)
    );
    if let Some(n) = max_commits {
        let _ = write!(url, "?max_commits={n}");
    }
    let client = reqwest::Client::new();
    let response = client.get(url).send().await.map_err(ApiError::Http)?;
    let status = response.status();
    if !status.is_success() {
        return Err(ApiError::from_response(response).await);
    }
    let text = response.text().await.map_err(ApiError::Http)?;
    serde_json::from_str::<DiffSourceOptions>(&text).map_err(ApiError::Serialization)
}

/// `POST /api/projects/:project_id/review/send`
pub async fn send_review(
    base_url: &str,
    project_id: &str,
    request: &SendReviewRequest,
) -> Result<SendReviewResponse, ApiError> {
    let url = format!(
        "{}/api/projects/{}/review/send",
        base_url.trim_end_matches('/'),
        percent_encode(project_id)
    );
    let client = reqwest::Client::new();
    let response = client
        .post(url)
        .json(request)
        .send()
        .await
        .map_err(ApiError::Http)?;
    let status = response.status();
    if !status.is_success() {
        return Err(ApiError::from_response(response).await);
    }
    let text = response.text().await.map_err(ApiError::Http)?;
    serde_json::from_str::<SendReviewResponse>(&text).map_err(ApiError::Serialization)
}

fn percent_encode(s: &str) -> String {
    use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
    const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    utf8_percent_encode(s, PATH_SEGMENT).to_string()
}

/// Wraps a byte stream and emits one `DiffEventWire` per newline-delimited
/// JSON record. Partial lines are buffered across chunks.
pub struct DiffEventStream {
    inner: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static>>,
    buffer: bytes::BytesMut,
    done: bool,
}

impl DiffEventStream {
    fn new(
        inner: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static>>,
    ) -> Self {
        Self {
            inner,
            buffer: bytes::BytesMut::new(),
            done: false,
        }
    }

    fn try_take_line(&mut self) -> Option<Vec<u8>> {
        if let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line = self.buffer.split_to(pos);
            // Drop the '\n' separator.
            let _ = self.buffer.split_to(1);
            Some(line.to_vec())
        } else {
            None
        }
    }
}

impl Stream for DiffEventStream {
    type Item = Result<DiffEventWire, ApiError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            // Emit buffered events first.
            if let Some(line) = self.try_take_line() {
                if line.is_empty() {
                    continue;
                }
                let decoded =
                    serde_json::from_slice::<DiffEventWire>(&line).map_err(ApiError::Serialization);
                return Poll::Ready(Some(decoded));
            }
            if self.done {
                return Poll::Ready(None);
            }
            match self.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    self.buffer.extend_from_slice(&bytes);
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(ApiError::Http(e))));
                }
                Poll::Ready(None) => {
                    self.done = true;
                    // Handle a trailing line without '\n'.
                    if !self.buffer.is_empty() {
                        let line = std::mem::take(&mut self.buffer).to_vec();
                        let decoded = serde_json::from_slice::<DiffEventWire>(&line)
                            .map_err(ApiError::Serialization);
                        return Poll::Ready(Some(decoded));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_event_started_decodes() {
        let line = br#"{"type":"started","files":[]}"#;
        let evt: DiffEventWire = serde_json::from_slice(line).unwrap();
        assert!(matches!(evt, DiffEventWire::Started { .. }));
    }

    #[test]
    fn diff_event_finished_decodes_with_error() {
        let line = br#"{"type":"finished","error":{"code":"timeout","message":"x"}}"#;
        let evt: DiffEventWire = serde_json::from_slice(line).unwrap();
        match evt {
            DiffEventWire::Finished { error: Some(e) } => {
                assert_eq!(e.code, zremote_protocol::project::DiffErrorCode::Timeout);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
