//! The three clipboard operations, and the snapshot that holds a paste
//! together.
//!
//! A paste is two round trips — `TARGETS`, then a read — and the clipboard is
//! free to change between them. The snapshot is what makes those two calls
//! describe one clipboard: `targets` takes it, `read` consults it and fills it
//! in, and `write` clears it, because a write is the clipboard it describes
//! ceasing to exist.
//!
//! The lock over it is taken twice rather than held across a subprocess. That
//! is the whole reason there is a task per request: holding it through the
//! fetch queued every other shell behind one `xclip`.

use super::{Agent, wedged, within};
use crate::protocol::{ErrorCode, MAX_PAYLOAD, Response};
use crate::resize;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// How long a `TARGETS` answer stays good for the read that follows it.
///
/// A paste is two round trips. Long enough to cover a slow link, short enough
/// that this is a snapshot for one paste rather than a cache of the clipboard.
pub const SNAPSHOT_TTL: Duration = Duration::from_secs(5);

pub(super) struct Snapshot {
    /// Which snapshot this is, counted by `Agent::snapshots`.
    ///
    /// A read consults the snapshot, drops the lock for the length of the
    /// subprocess, and re-takes it to store what it fetched — so by then the
    /// snapshot it consulted may have been replaced by a concurrent `TARGETS`
    /// or cleared by a write. Storing under the new one would serve a later
    /// paste the bytes of an earlier clipboard, which is the one failure worse
    /// than a slow paste. Compared by sequence rather than by `taken`, because
    /// two snapshots taken in the same instant are a fiction only in
    /// production.
    seq: u64,
    taken: Instant,
    types: Vec<String>,
    /// Filled lazily: `TARGETS` records what was advertised, and the read that
    /// follows stores the bytes it fetched under that advertisement.
    content: Vec<(String, Vec<u8>)>,
}

impl Agent {
    /// The one operation that changes the laptop rather than reporting on it.
    pub(super) async fn write(
        &self,
        mime: &str,
        body: Option<Vec<u8>>,
        len: usize,
    ) -> (Response, Option<Vec<u8>>) {
        let bytes = body.unwrap_or_default();
        if bytes.len() != len {
            return (
                Response::Error {
                    code: ErrorCode::BadRequest,
                    message: format!(
                        "the write announced {len} bytes and carried {}",
                        bytes.len()
                    ),
                },
                None,
            );
        }

        // The snapshot describes a clipboard that is about to stop existing.
        // Dropped before the write rather than after, so a write that fails
        // half-way cannot leave a reader being served content the laptop no
        // longer holds.
        *self.snapshot.lock().await = None;

        let Some(written) = within(self.clipboard.write(mime, &bytes)).await else {
            return (wedged("write the laptop's clipboard"), None);
        };
        match written {
            Ok(true) => (Response::Written, None),
            Ok(false) => (
                Response::Error {
                    code: ErrorCode::Unsupported,
                    message: format!("the channel does not carry `{mime}`"),
                },
                None,
            ),
            Err(error) => (
                Response::Error {
                    code: ErrorCode::Internal,
                    message: format!("could not write the laptop's clipboard: {error}"),
                },
                None,
            ),
        }
    }

    pub(super) async fn targets(&self, now: Instant) -> (Response, Option<Vec<u8>>) {
        let Some(listed) = within(self.clipboard.targets()).await else {
            return (wedged("list what is on the laptop's clipboard"), None);
        };
        let types = match listed {
            Ok(types) => types,
            Err(error) => return (internal(error), None),
        };

        *self.snapshot.lock().await = Some(Snapshot {
            seq: self.snapshots.fetch_add(1, Ordering::Relaxed),
            taken: now,
            types: types.clone(),
            content: Vec::new(),
        });

        (Response::Targets(types), None)
    }

    /// One read, with the snapshot lock taken twice rather than held across the
    /// subprocess.
    ///
    /// **Holding it through the fetch defeated the reason there is a task per
    /// request.** `serve_pipe` and `agent::server` both spawn one, on the
    /// stated grounds that "a slow clipboard read must not hold up every other
    /// shell on that server" — and then every one of them queued behind a
    /// single `xclip`, including the pings and the writes that never look at
    /// the snapshot at all. One developer pasting a screenshot stalled every
    /// other shell into that server for as long as the resize took.
    ///
    /// Consult, drop, fetch, re-lock to store. What that costs is a duplicate
    /// fetch when two reads of the same type race, which is a second `xclip`
    /// and nothing else; what it buys is that the lock is never held across
    /// anything that can block.
    pub(super) async fn read(&self, mime: &str, now: Instant) -> (Response, Option<Vec<u8>>) {
        // First visit: expire, serve from the snapshot if it already holds
        // these bytes, and remember what it advertised.
        let (seq, advertised) = {
            let mut snapshot = self.snapshot.lock().await;

            // Expire first, so a stale snapshot never answers.
            if snapshot
                .as_ref()
                .is_some_and(|held| now.duration_since(held.taken) > SNAPSHOT_TTL)
            {
                *snapshot = None;
            }

            if let Some(held) = snapshot.as_ref()
                && let Some((_, bytes)) = held.content.iter().find(|(t, _)| t == mime)
            {
                return payload(mime, bytes.clone());
            }

            match snapshot.as_ref() {
                Some(held) => (Some(held.seq), held.types.iter().any(|t| t == mime)),
                None => (None, false),
            }
        };

        // No lock is held from here to the re-take below. That is the whole
        // point of splitting it.
        let Some(read) = within(self.clipboard.read(mime)).await else {
            return (wedged("read the laptop's clipboard"), None);
        };
        let fetched = match read {
            Ok(found) => found,
            Err(error) => return (internal(error), None),
        };

        let Some(bytes) = fetched else {
            // The clipboard moved between the advertisement and the read. The
            // caller is mid-paste, so say what happened rather than reporting
            // an empty clipboard it can do nothing with.
            let message = if advertised {
                format!("the clipboard changed while `{mime}` was being read")
            } else {
                "no clipboard content of that type".to_string()
            };
            return (
                Response::Error {
                    code: ErrorCode::Unavailable,
                    message,
                },
                None,
            );
        };

        // Lanczos3 over a 24-megapixel screenshot is seconds of pure CPU — the
        // crate's own fixtures say half a minute in a debug build — and the
        // runtime is current-thread. Run here it stalls every other future on
        // the reactor, including the pipe reader that has to answer the pump's
        // keepalive inside forty-five seconds: a big paste ended the channel
        // that was carrying it. `filelock` already uses this shape for the
        // blocking `flock` it cannot make async.
        let owned = mime.to_string();
        let Ok(bytes) =
            tokio::task::spawn_blocking(move || resize::to_ceiling(&owned, bytes)).await
        else {
            // The blocking pool went away, which is the runtime shutting down.
            // The bytes went with the task, so there is nothing to fall back to.
            return (
                Response::Error {
                    code: ErrorCode::Internal,
                    message: "the laptop could not prepare the image for the channel".to_string(),
                },
                None,
            );
        };

        // Re-taken to store, and only under the snapshot this read consulted.
        // A `TARGETS` that landed while the subprocess ran describes a
        // different clipboard, and filing these bytes under it would serve the
        // next paste the previous one's content.
        if let Some(held) = self.snapshot.lock().await.as_mut()
            && Some(held.seq) == seq
        {
            held.content.push((mime.to_string(), bytes.clone()));
        }

        payload(mime, bytes)
    }
}

fn internal(error: anyhow::Error) -> Response {
    Response::Error {
        code: ErrorCode::Internal,
        message: format!("could not read the laptop's clipboard: {error}"),
    }
}

fn payload(mime: &str, bytes: Vec<u8>) -> (Response, Option<Vec<u8>>) {
    if bytes.len() > MAX_PAYLOAD {
        return (
            Response::Error {
                code: ErrorCode::TooLarge,
                message: format!(
                    "`{mime}` is {} bytes, over the {MAX_PAYLOAD} byte channel limit",
                    bytes.len()
                ),
            },
            None,
        );
    }
    (Response::Payload { len: bytes.len() }, Some(bytes))
}
