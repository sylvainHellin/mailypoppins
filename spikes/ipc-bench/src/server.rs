//! The daemon side: a tokio Unix-socket listener speaking NDJSON JSON-RPC.
//!
//! One connection at a time, requests answered in order. The store handle
//! lives in the connection task and is never shared, which is the constraint
//! `docs/plans/preview-latency.md` records for `rusqlite::Connection` (`Send`,
//! not `Sync`). Store reads are blocking calls made from an async task on
//! purpose: the spike measures transport, and a `spawn_blocking` hop would add
//! a scheduler cost that a per-account runtime would not pay the same way.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::proto::{
    ChunkFrame, ChunkParams, Delivery, Handle, Meta, RawResponse, Request, CHUNK_BYTES, CHUNK_ROWS,
    HANDLE_TTL_SECS, MAX_REQUEST_FRAME, METHOD_CHUNK, METHOD_RUN,
};
use crate::work::{Fixture, Workload};

/// Bind the socket before any client exists, so the harness never races the
/// listener. Returns the listener and the path it is bound to.
pub fn bind(dir: &std::path::Path) -> Result<(UnixListener, PathBuf)> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("daemon.sock");
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("clearing {}", path.display()))?;
    }
    let listener =
        UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    Ok((listener, path))
}

/// Serve exactly one connection, then return. The harness is one client.
pub async fn serve_one(listener: UnixListener, fixture: Fixture, handle_dir: PathBuf) -> Result<()> {
    let (stream, _) = listener.accept().await.context("accepting the client")?;
    handle(stream, fixture, handle_dir).await
}

async fn handle(stream: UnixStream, fixture: Fixture, handle_dir: PathBuf) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    let mut last_handle: Option<PathBuf> = None;
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await.context("reading a frame")?;
        if n == 0 {
            return Ok(()); // client hung up: end of run
        }
        if n > MAX_REQUEST_FRAME {
            // -32004 frame_too_large in the real protocol; the spike never
            // sends a request this big, so a hard error is the honest answer.
            return Err(anyhow!("request frame of {n} bytes over the {MAX_REQUEST_FRAME} cap"));
        }
        let req: Request = serde_json::from_str(line.trim_end()).context("parsing a request")?;
        if req.method != METHOD_RUN {
            return Err(anyhow!("unknown method {}", req.method));
        }
        let workload: Workload = req.params.workload.parse()?;

        // Dispatch: the store read and the row conversion, i.e. exactly the
        // work the direct call also does.
        let started = Instant::now();
        let mut result = fixture.run(workload)?;
        let dispatch_us = started.elapsed().as_micros() as u64;

        let mut bytes = 0u64;
        let mut serialize_us = 0u64;
        let mut handle_write_us = 0u64;
        let mut handle_bytes = 0u64;

        match req.params.delivery {
            Delivery::Single => {}
            Delivery::Chunked => {
                // N chunk notifications, then a response whose result carries
                // only the counts.
                let mut seq = 0u32;
                let rows = std::mem::take(&mut result.rows);
                for chunk in rows.chunks(CHUNK_ROWS) {
                    let params =
                        ChunkParams { id: req.id, seq, rows: chunk.to_vec(), ..Default::default() };
                    seq += 1;
                    emit_chunk(&mut write, params, &mut serialize_us, &mut bytes).await?;
                }
                let compact = std::mem::take(&mut result.compact);
                for chunk in compact.chunks(CHUNK_ROWS) {
                    let params = ChunkParams {
                        id: req.id,
                        seq,
                        compact: chunk.to_vec(),
                        ..Default::default()
                    };
                    seq += 1;
                    emit_chunk(&mut write, params, &mut serialize_us, &mut bytes).await?;
                }
                if let Some(body) = result.body.take() {
                    for slice in slices(&body, CHUNK_BYTES) {
                        let params = ChunkParams {
                            id: req.id,
                            seq,
                            text: Some(slice.to_string()),
                            ..Default::default()
                        };
                        seq += 1;
                        emit_chunk(&mut write, params, &mut serialize_us, &mut bytes).await?;
                    }
                }
                result.chunks = seq;
            }
            Delivery::Handle => {
                // The payload is materialised where the client can read it,
                // and the frame carries only the pointer. Rows go out as
                // NDJSON, one record per line, which is the ordering contract
                // `mp dump-mailbox --json` already has; a body goes out as its
                // own bytes, unescaped.
                let t = Instant::now();
                let file = match result.body.take() {
                    // A body travels as its own bytes: no JSON escaping, and
                    // the envelope stays inline in the frame, which is what a
                    // `message.body` handle would look like.
                    Some(body) => body,
                    None => {
                        let mut ndjson = String::new();
                        for row in result.rows.drain(..) {
                            ndjson
                                .push_str(&serde_json::to_string(&row).context("encoding a record")?);
                            ndjson.push('\n');
                        }
                        for row in result.compact.drain(..) {
                            ndjson
                                .push_str(&serde_json::to_string(&row).context("encoding a record")?);
                            ndjson.push('\n');
                        }
                        ndjson
                    }
                };
                serialize_us += t.elapsed().as_micros() as u64;

                let t = Instant::now();
                let path = handle_dir.join(format!("handle-{}-{}.ndjson", std::process::id(), req.id));
                std::fs::write(&path, file.as_bytes())
                    .with_context(|| format!("writing the handle file {}", path.display()))?;
                handle_write_us = t.elapsed().as_micros() as u64;
                handle_bytes = file.len() as u64;

                // Crude expiry: the previous handle goes when the next one is
                // written, so a run leaves one file behind and not `samples`.
                if let Some(stale) = last_handle.replace(path.clone()) {
                    let _ = std::fs::remove_file(stale);
                }
                result.handle = Some(Handle {
                    path: path.to_string_lossy().into_owned(),
                    expires_at: expires_at(),
                    bytes: handle_bytes,
                });
            }
        }

        let t = Instant::now();
        let payload = serde_json::value::to_raw_value(&result).context("encoding the result")?;
        serialize_us += t.elapsed().as_micros() as u64;
        bytes += payload.get().len() as u64;

        let response = RawResponse {
            jsonrpc: "2.0",
            id: req.id,
            result: &payload,
            meta: Meta { dispatch_us, serialize_us, bytes, handle_write_us, handle_bytes },
        };
        // The envelope itself is a handful of bytes around an already-encoded
        // payload, so its encoding is not charged to `serialize_us`.
        let encoded = serde_json::to_string(&response).context("encoding the response")?;
        write_line(&mut write, &encoded).await?;
    }
}

/// Encode one chunk notification, charge its encode to `serialize_us` and its
/// payload to `bytes`, and put it on the wire.
async fn emit_chunk(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    params: ChunkParams,
    serialize_us: &mut u64,
    bytes: &mut u64,
) -> Result<()> {
    let frame =
        ChunkFrame { jsonrpc: "2.0".into(), method: METHOD_CHUNK.into(), params };
    let t = Instant::now();
    let encoded = serde_json::to_string(&frame).context("encoding a chunk")?;
    *serialize_us += t.elapsed().as_micros() as u64;
    *bytes += encoded.len() as u64;
    write_line(write, &encoded).await
}

/// Split a body into slices of at most `size` bytes, never inside a character.
fn slices(body: &str, size: usize) -> Vec<&str> {
    let mut out = Vec::with_capacity(body.len() / size + 1);
    let mut start = 0usize;
    while start < body.len() {
        let mut end = (start + size).min(body.len());
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        out.push(&body[start..end]);
        start = end;
    }
    out
}

/// When the daemon stops promising the handle file is there.
fn expires_at() -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(HANDLE_TTL_SECS)).to_rfc3339()
}

/// NDJSON framing, write side: the payload, then the one delimiter byte.
async fn write_line(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    payload: &str,
) -> Result<()> {
    debug_assert!(!payload.contains('\n'), "a frame must not embed a raw newline");
    write.write_all(payload.as_bytes()).await.context("writing a frame")?;
    write.write_all(b"\n").await.context("writing the frame delimiter")?;
    write.flush().await.context("flushing a frame")?;
    Ok(())
}
