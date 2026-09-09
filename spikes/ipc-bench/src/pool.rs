//! P1a-U5: what a per-account read pool of 1, 2 or 4 connections does to the
//! preview read while the account is busy.
//!
//! The shape under test is the one `docs/plans/preview-latency.md` forces:
//! `rusqlite::Connection` is `Send` but not `Sync`, so a pool is N connections
//! each owned by one thread, never one connection shared behind a lock. Each
//! worker here owns its own [`Fixture`], and the async side hands work to a
//! free worker over a channel.
//!
//! Load, all of it running against the same account store at once:
//!
//! - the measured client, asking for W1 (one envelope plus its body) in a loop;
//! - a load client, asking for the whole 5000-row `Bulk` listing back to back;
//! - a writer thread committing sync-like transactions on its own connection.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{oneshot, Semaphore};

use crate::proto::{Delivery, Meta, RawResponse, Request, WorkResult, METHOD_RUN};
use crate::stats::Stat;
use crate::work::{Fixture, Workload, ACCOUNT, W2_MAILBOX};

/// Rows one sync-like transaction touches: 200 updated, 50 inserted. A poll of
/// a busy mailbox writes about this much.
const WRITE_UPDATE_ROWS: i64 = 200;
const WRITE_INSERT_ROWS: i64 = 50;

/// Where the writer's inserted rows go, so no measured listing sees them.
const SCRATCH_MAILBOX: &str = "SyncScratch";

#[derive(Serialize)]
pub struct PoolReport {
    pub scenario: &'static str,
    /// Read connections the server holds for the account.
    pub pool: usize,
    /// Whether the sync-like writer ran beside the measured client.
    pub contend: bool,
    /// Whether the 5000-row list load ran beside it too.
    pub load: bool,
    pub samples: usize,
    pub preview_p50_us: f64,
    pub preview_p95_us: f64,
    pub preview_max_us: f64,
    /// Whole-list round trips the load client completed during the run.
    pub load_samples: usize,
    pub load_p50_us: f64,
    pub load_p95_us: f64,
    /// Transactions the writer committed during the run.
    pub writes: u64,
    pub write_p50_us: f64,
    pub write_p95_us: f64,
}

/// Run the read-pool scenario end to end and report it.
pub async fn run(
    fixture_root: &Path,
    pool: usize,
    contend: bool,
    load_client: bool,
    samples: usize,
    warmup: usize,
) -> Result<PoolReport> {
    if pool == 0 {
        return Err(anyhow!("--pool must be at least 1"));
    }
    let run_dir = std::env::temp_dir().join(format!("ipc-bench-pool-{}", std::process::id()));
    let (listener, sock) = crate::server::bind(&run_dir)?;

    let workers = Workers::spawn(fixture_root, pool)?;
    let server_workers = workers.clone();
    let clients = if load_client { 2 } else { 1 };
    let server = tokio::spawn(async move { serve(listener, server_workers, clients).await });

    let mut preview = crate::client::Client::connect(&sock).await?;
    let mut load = if load_client {
        Some(crate::client::Client::connect(&sock).await?)
    } else {
        None
    };

    // The writer runs on its own connection, opened read-write on the same
    // database: WAL means it does not block the readers, which is exactly the
    // claim this run is testing.
    let writer = if contend { Some(Writer::start(fixture_root)?) } else { None };

    for _ in 0..warmup {
        preview.sample(Workload::W1, Delivery::Single).await?;
        if let Some(load) = load.as_mut() {
            load.sample(Workload::Whole, Delivery::Single).await?;
        }
    }

    // The load client runs as its own task so the two clients are genuinely
    // concurrent: a sequential loop would never put a list read in flight
    // while the preview is waiting, which is the whole question.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let load_task = load.map(|mut load| {
        let stop = stop.clone();
        tokio::spawn(async move {
            let mut times = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                match load.sample(Workload::Whole, Delivery::Single).await {
                    Ok(s) => times.push(s.total),
                    Err(e) => return Err(e),
                }
            }
            Ok::<_, anyhow::Error>((load, times))
        })
    });

    let mut totals = Vec::with_capacity(samples);
    for _ in 0..samples {
        let s = preview.sample(Workload::W1, Delivery::Single).await?;
        totals.push(s.total);
    }

    stop.store(true, Ordering::Relaxed);
    let (load_samples, mut load_times) = match load_task {
        Some(task) => {
            let (load, times) = task.await.context("joining the load client")??;
            drop(load);
            (times.len(), times)
        }
        None => (0, Vec::new()),
    };
    let (writes, mut write_times) = match writer {
        Some(writer) => writer.stop()?,
        None => (0, Vec::new()),
    };

    drop(preview);
    server.await.context("joining the pool server")??;
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_dir(&run_dir);

    let head = Stat::of(&mut totals);
    let load_stat = Stat::of(&mut load_times);
    let write_stat = Stat::of(&mut write_times);
    Ok(PoolReport {
        scenario: "read-pool",
        pool,
        contend,
        load: load_client,
        samples,
        preview_p50_us: head.p50_us,
        preview_p95_us: head.p95_us,
        preview_max_us: head.max_us,
        load_samples,
        load_p50_us: load_stat.p50_us,
        load_p95_us: load_stat.p95_us,
        writes,
        write_p50_us: write_stat.p50_us,
        write_p95_us: write_stat.p95_us,
    })
}

/// One job for a read connection: a workload, and where its answer goes.
type Job = (Workload, oneshot::Sender<Result<(WorkResult, u64), String>>);

/// The read pool: N threads, each owning one `Fixture` and therefore one
/// `rusqlite::Connection`, plus a free list the async side waits on.
#[derive(Clone)]
struct Workers {
    senders: Arc<Vec<std::sync::mpsc::Sender<Job>>>,
    /// One permit per idle worker: acquiring it is what makes a caller wait
    /// when every connection is busy, which is the queueing a pool of one has
    /// and a pool of four mostly does not.
    permits: Arc<Semaphore>,
    idle: Arc<Mutex<Vec<usize>>>,
}

impl Workers {
    fn spawn(fixture_root: &Path, pool: usize) -> Result<Self> {
        let mut senders = Vec::with_capacity(pool);
        for i in 0..pool {
            let fixture = Fixture::open(fixture_root)?;
            let (send, jobs) = std::sync::mpsc::channel::<Job>();
            std::thread::Builder::new()
                .name(format!("read-{i}"))
                .spawn(move || {
                    while let Ok((workload, reply)) = jobs.recv() {
                        let t = Instant::now();
                        let answer = fixture
                            .run(workload)
                            .map(|r| (r, t.elapsed().as_micros() as u64))
                            .map_err(|e| format!("{e:#}"));
                        let _ = reply.send(answer);
                    }
                })
                .with_context(|| format!("spawning read worker {i}"))?;
            senders.push(send);
        }
        Ok(Self {
            senders: Arc::new(senders),
            permits: Arc::new(Semaphore::new(pool)),
            idle: Arc::new(Mutex::new((0..pool).collect())),
        })
    }

    /// Hand one workload to a free connection and wait for its answer.
    async fn dispatch(&self, workload: Workload) -> Result<(WorkResult, u64)> {
        let permit = self.permits.acquire().await.context("acquiring a read connection")?;
        let worker = self.idle.lock().unwrap().pop().expect("a permit means a free worker");
        let (reply, answer) = oneshot::channel();
        self.senders[worker].send((workload, reply)).context("handing work to a read worker")?;
        let out = answer.await.context("waiting on a read worker")?;
        self.idle.lock().unwrap().push(worker);
        drop(permit);
        out.map_err(|e| anyhow!(e))
    }
}

/// The pool server: many connections, every read going through the pool.
///
/// Deliberately a simpler handler than [`crate::server`]: this scenario asks
/// only about queueing, so there is no chunking and no handle path here.
async fn serve(listener: UnixListener, workers: Workers, clients: usize) -> Result<()> {
    let mut tasks = Vec::with_capacity(clients);
    // The client count is known: the measured client, plus the load client
    // when contention is on. The run ends when they have all hung up.
    for _ in 0..clients {
        let (stream, _) = listener.accept().await.context("accepting a client")?;
        let workers = workers.clone();
        tasks.push(tokio::spawn(async move { handle(stream, workers).await }));
    }
    for task in tasks {
        task.await.context("joining a connection task")??;
    }
    Ok(())
}

async fn handle(stream: UnixStream, workers: Workers) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.context("reading a frame")? == 0 {
            return Ok(());
        }
        let req: Request = serde_json::from_str(line.trim_end()).context("parsing a request")?;
        if req.method != METHOD_RUN {
            return Err(anyhow!("unknown method {}", req.method));
        }
        let workload: Workload = req.params.workload.parse()?;
        let (result, dispatch_us) = workers.dispatch(workload).await?;

        let t = Instant::now();
        let payload = serde_json::value::to_raw_value(&result).context("encoding the result")?;
        let serialize_us = t.elapsed().as_micros() as u64;
        let response = RawResponse {
            jsonrpc: "2.0",
            id: req.id,
            result: &payload,
            meta: Meta {
                dispatch_us,
                serialize_us,
                bytes: payload.get().len() as u64,
                ..Meta::default()
            },
        };
        let encoded = serde_json::to_string(&response).context("encoding the response")?;
        write.write_all(encoded.as_bytes()).await.context("writing a frame")?;
        write.write_all(b"\n").await.context("writing the delimiter")?;
        write.flush().await.context("flushing a frame")?;
    }
}

/// The sync-like writer: its own connection, transactions back to back.
struct Writer {
    stop: Arc<std::sync::atomic::AtomicBool>,
    commits: Arc<AtomicU64>,
    join: std::thread::JoinHandle<Result<Vec<f64>>>,
}

impl Writer {
    fn start(fixture_root: &Path) -> Result<Self> {
        let db: PathBuf = fixture_root
            .join("data")
            .join("accounts")
            .join(ACCOUNT)
            .join("store.sqlite3");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let commits = Arc::new(AtomicU64::new(0));
        let (thread_stop, thread_commits) = (stop.clone(), commits.clone());
        let join = std::thread::Builder::new()
            .name("sync-writer".into())
            .spawn(move || -> Result<Vec<f64>> {
                let store = mailypoppins::store::Store::open(&db)
                    .with_context(|| format!("opening {} read-write", db.display()))?;
                let mut times = Vec::new();
                let mut band: i64 = 0;
                while !thread_stop.load(Ordering::Relaxed) {
                    let t = Instant::now();
                    commit_one(&store, band)?;
                    times.push(t.elapsed().as_secs_f64() * 1e6);
                    thread_commits.fetch_add(1, Ordering::Relaxed);
                    band = band.wrapping_add(1);
                }
                Ok(times)
            })
            .context("spawning the sync writer")?;
        Ok(Self { stop, commits, join })
    }

    /// Stop the writer and collect its commit count and per-commit timings.
    fn stop(self) -> Result<(u64, Vec<f64>)> {
        self.stop.store(true, Ordering::Relaxed);
        let times = self.join.join().map_err(|_| anyhow!("the sync writer panicked"))??;
        Ok((self.commits.load(Ordering::Relaxed), times))
    }
}

/// One transaction: flip `\Seen` on a band of real rows, then insert a batch of
/// new ones. Both halves are what a mailbox poll writes, and the update is the
/// half that dirties pages the readers are reading.
fn commit_one(store: &mailypoppins::store::Store, band: i64) -> Result<()> {
    let first = (band * WRITE_UPDATE_ROWS) % 5000;
    let uid_base = 1_000_000 + (band % 20) * WRITE_INSERT_ROWS;
    let mut sql = String::from("BEGIN IMMEDIATE;\n");
    sql.push_str(&format!(
        "UPDATE messages SET flags = CASE WHEN flags LIKE '%\\Seen%' THEN NULL ELSE '\\Seen' END \
         WHERE account = '{ACCOUNT}' AND mailbox = '{W2_MAILBOX}' \
           AND id >= {first} AND id < {};\n",
        first + WRITE_UPDATE_ROWS
    ));
    for i in 0..WRITE_INSERT_ROWS {
        let uid = uid_base + i;
        sql.push_str(&format!(
            "INSERT OR REPLACE INTO messages \
             (account, mailbox, uid, message_id, subject, date_sort, date_display, flags) VALUES \
             ('{ACCOUNT}', '{SCRATCH_MAILBOX}', {uid}, '<scratch-{uid}@fixture.invalid>', \
              'sync write {band}', {uid}, '2026-01-01T00:00:00Z', NULL);\n"
        ));
    }
    sql.push_str("COMMIT;\n");
    store.conn().execute_batch(&sql).context("committing a sync-like transaction")?;
    Ok(())
}

pub fn print_human(r: &PoolReport) {
    println!(
        "read pool {} | {} samples | writer {} | list load {}",
        r.pool,
        r.samples,
        if r.contend { "on" } else { "off" },
        if r.load { "on" } else { "off" }
    );
    println!(
        "preview w1  p50 {:>10.2} us  p95 {:>10.2} us  max {:>10.2} us",
        r.preview_p50_us, r.preview_p95_us, r.preview_max_us
    );
    println!(
        "list load   {} round trips, p50 {:.2} us, p95 {:.2} us",
        r.load_samples, r.load_p50_us, r.load_p95_us
    );
    println!(
        "sync writes {} commits, p50 {:.2} us, p95 {:.2} us",
        r.writes, r.write_p50_us, r.write_p95_us
    );
}
