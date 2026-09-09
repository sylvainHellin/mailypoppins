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
    ChunkFrame, ChunkParams, Meta, RawResponse, Request, CHUNK_ROWS, MAX_REQUEST_FRAME, METHOD_CHUNK,
    METHOD_RUN,
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
pub async fn serve_one(listener: UnixListener, fixture: Fixture) -> Result<()> {
    let (stream, _) = listener.accept().await.context("accepting the client")?;
    handle(stream, fixture).await
}

async fn handle(stream: UnixStream, fixture: Fixture) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
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

        if req.params.stream {
            // Streamed answer: N chunk notifications, then a response whose
            // result carries only the counts.
            let rows = std::mem::take(&mut result.rows);
            let mut seq = 0u32;
            for chunk in rows.chunks(CHUNK_ROWS) {
                let frame = ChunkFrame {
                    jsonrpc: "2.0".into(),
                    method: METHOD_CHUNK.into(),
                    params: ChunkParams { id: req.id, seq, rows: chunk.to_vec() },
                };
                let t = Instant::now();
                let encoded = serde_json::to_string(&frame).context("encoding a chunk")?;
                serialize_us += t.elapsed().as_micros() as u64;
                bytes += encoded.len() as u64;
                write_line(&mut write, &encoded).await?;
                seq += 1;
            }
            result.chunks = seq;
        }

        let t = Instant::now();
        let payload = serde_json::value::to_raw_value(&result).context("encoding the result")?;
        serialize_us += t.elapsed().as_micros() as u64;
        bytes += payload.get().len() as u64;

        let response = RawResponse {
            jsonrpc: "2.0",
            id: req.id,
            result: &payload,
            meta: Meta { dispatch_us, serialize_us, bytes },
        };
        // The envelope itself is a handful of bytes around an already-encoded
        // payload, so its encoding is not charged to `serialize_us`.
        let encoded = serde_json::to_string(&response).context("encoding the response")?;
        write_line(&mut write, &encoded).await?;
    }
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
