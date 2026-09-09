//! The client side, timed stage by stage.
//!
//! The five stages the unit asks for, in the order a sample pays them:
//!
//! - `serialize`    encoding the request struct into JSON;
//! - `frame_write`  writing the payload, the `\n` delimiter and the flush;
//! - `round_trip`   from the flush to the last byte of the last frame read,
//!   which contains the server's own dispatch and serialise;
//! - `dispatch`     the server's store read, reported back in the response;
//! - `deserialize`  decoding the frames into typed values, client side.
//!
//! Read and decode are deliberately not interleaved: every frame is read as
//! bytes first, then all of them are decoded, so the two stages can be charged
//! separately. A production client would overlap them, which makes the split
//! an upper bound on each stage and not on their sum.

use anyhow::{anyhow, Context, Result};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::proto::{ChunkFrame, Request, Response};
use crate::work::Workload;

/// One sample's stage timings, in microseconds.
#[derive(Debug, Default, Clone, Copy)]
pub struct Sample {
    pub serialize: f64,
    pub frame_write: f64,
    pub round_trip: f64,
    pub dispatch: f64,
    pub server_serialize: f64,
    pub deserialize: f64,
    /// Sum of the client-observed stages: what a caller waits for.
    pub total: f64,
    /// Frames the answer took, response included.
    pub frames: u32,
    /// Payload bytes of the answer, delimiters excluded.
    pub bytes: u64,
}

pub struct Client {
    write: tokio::net::unix::OwnedWriteHalf,
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    next_id: u64,
    /// Reused across samples so the measurement is not an allocator benchmark.
    frames: Vec<String>,
}

impl Client {
    pub async fn connect(path: &std::path::Path) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .await
            .with_context(|| format!("connecting to {}", path.display()))?;
        let (read, write) = stream.into_split();
        Ok(Self { write, reader: BufReader::new(read), next_id: 1, frames: Vec::new() })
    }

    /// One request/response exchange, timed.
    pub async fn sample(&mut self, workload: Workload) -> Result<Sample> {
        let id = self.next_id;
        self.next_id += 1;
        let request = Request::new(id, workload.as_str(), workload.streams());

        let t0 = Instant::now();
        let payload = serde_json::to_string(&request).context("encoding the request")?;
        let t1 = Instant::now();
        self.write.write_all(payload.as_bytes()).await.context("writing the request")?;
        self.write.write_all(b"\n").await.context("writing the delimiter")?;
        self.write.flush().await.context("flushing the request")?;
        let t2 = Instant::now();

        // Read frames until the response frame arrives. A chunk notification
        // has no `id` at the top level, which is how the reader tells them
        // apart without decoding: cheap, and the same test a real client makes.
        self.frames.clear();
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).await.context("reading a frame")?;
            if n == 0 {
                return Err(anyhow!("the server closed the connection mid-answer"));
            }
            let is_response = !line.starts_with("{\"jsonrpc\":\"2.0\",\"method\":");
            self.frames.push(line);
            if is_response {
                break;
            }
        }
        let t3 = Instant::now();

        let mut response: Option<Response> = None;
        let mut rows = 0usize;
        for frame in &self.frames {
            let text = frame.trim_end();
            if text.starts_with("{\"jsonrpc\":\"2.0\",\"method\":") {
                let chunk: ChunkFrame = serde_json::from_str(text).context("decoding a chunk")?;
                rows += chunk.params.rows.len();
            } else {
                let decoded: Response =
                    serde_json::from_str(text).context("decoding the response")?;
                rows += decoded.result.rows.len();
                response = Some(decoded);
            }
        }
        let t4 = Instant::now();

        let response = response.expect("the loop breaks only on a response frame");
        if response.result.row_count as usize != rows {
            return Err(anyhow!(
                "the answer carried {rows} rows but claims {}",
                response.result.row_count
            ));
        }

        let us = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1e6;
        Ok(Sample {
            serialize: us(t0, t1),
            frame_write: us(t1, t2),
            round_trip: us(t2, t3),
            dispatch: response.meta.dispatch_us as f64,
            server_serialize: response.meta.serialize_us as f64,
            deserialize: us(t3, t4),
            total: us(t0, t4),
            frames: self.frames.len() as u32,
            bytes: response.meta.bytes,
        })
    }

    /// The last answer's frames, for the framing probe.
    pub fn last_frames(&self) -> &[String] {
        &self.frames
    }
}
