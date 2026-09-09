//! P1a-U1: what a JSON-RPC-over-Unix-socket round trip costs against the same
//! work done as a direct library call.
//!
//! Both sides read the same fixture store through the same product API
//! (`mailypoppins::store::read`), so every microsecond of difference is
//! transport: request encode, framing, socket, dispatch, response decode.
//! Run recipe and output shape: see README.md.

mod client;
mod pool;
mod proto;
mod server;
mod stats;
mod work;

use anyhow::{Context, Result};
use clap::Parser;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use crate::proto::Delivery;
use crate::stats::Stat;
use crate::work::{Fixture, Workload};

#[derive(Parser)]
#[command(about = "Measure NDJSON JSON-RPC over a Unix socket against a direct library call")]
struct Args {
    /// Workload or scenario to run: w1 (preview), w2 (5000-row list), w3
    /// (whole-account stream), w4 (10 MiB body), whole / page / count / jump /
    /// filter / select_all (P1a-U3), or the composites `paged-sync` (page +
    /// count) and `paged` (page + count + jump + filter + select_all).
    #[arg(long, default_value = "w1")]
    workload: String,
    /// Timed samples. The heavy workloads want fewer.
    #[arg(long, default_value_t = 2000)]
    samples: usize,
    /// Untimed samples run first, to warm the page cache and the SQLite
    /// statement cache.
    #[arg(long, default_value_t = 20)]
    warmup: usize,
    /// Fixture root, as passed to `mkfixture --out`. Built on demand when
    /// absent.
    #[arg(long, default_value = "/tmp/mp-ipc-fixture")]
    fixture: PathBuf,
    /// Machine-readable output on stdout, one JSON object.
    #[arg(long)]
    json: bool,
    /// How the answer travels (P1a-U4): `single` frame, `chunked` frames, or a
    /// temp-file `handle` the client reads. Default: `chunked` for w3, the
    /// whole-account dump, and `single` for everything else.
    #[arg(long)]
    delivery: Option<String>,
    /// Where a `handle` delivery materialises its file. The default is the
    /// system temp dir, which is tmpfs on this host; point it at a real disk to
    /// measure the handle without the page cache doing the work.
    #[arg(long)]
    handle_dir: Option<PathBuf>,
    /// P1a-U5: run the read-pool scenario with this many read connections
    /// instead of the direct-versus-socket A/B.
    #[arg(long)]
    pool: Option<usize>,
    /// P1a-U5: put a 5000-row list load and a sync-like writer in flight
    /// beside the measured preview reads.
    #[arg(long)]
    contend: bool,
    /// P1a-U5 control: contend with the writer only, no list load, which
    /// separates what the WAL writer costs a reader from what the reader queue
    /// costs it.
    #[arg(long)]
    writer_only: bool,
    /// Run the direct call on a dedicated OS thread outside the tokio runtime
    /// instead of inline on the runtime's main task.
    #[arg(long)]
    direct_thread: bool,
    /// Take the direct sample before the round trip in each iteration, instead
    /// of after it.
    #[arg(long)]
    direct_first: bool,
}

/// The contract shape: the top-level figures are the client-observed round
/// trip, `stages` breaks them down and carries the direct call beside them.
#[derive(Serialize)]
struct Report {
    workload: String,
    /// How the answer travelled: `single`, `chunked` or `handle`.
    delivery: &'static str,
    samples: usize,
    p50_us: f64,
    p95_us: f64,
    max_us: f64,
    stages: BTreeMap<String, Stat>,
    framing: Framing,
    /// Payload bytes of one answer, delimiters excluded. A composite scenario
    /// sums its steps.
    bytes: u64,
    /// Frames one answer took, response frame included. Summed over the steps.
    frames: u32,
    /// Bytes a `handle` delivery put in the file instead of on the wire.
    file_bytes: u64,
    /// Payload bytes moved per second at the p50 round trip, wire and file
    /// together, in MB/s (decimal, matching `bytes`).
    throughput_mbps: f64,
    /// One entry per round trip of a composite scenario; a single-step run has
    /// one entry that repeats the head figures.
    steps: Vec<StepReport>,
    /// Where the direct call ran: `inline` on the runtime, or `thread` on a
    /// dedicated OS thread outside it.
    direct_mode: &'static str,
    /// Which side of the pair each iteration measured first.
    order: &'static str,
}

/// One round trip of a scenario, so a composite figure can be read back to the
/// method that produced it.
#[derive(Serialize)]
struct StepReport {
    method: String,
    bytes: u64,
    frames: u32,
    p50_us: f64,
    p95_us: f64,
}

/// What the `\n` delimiter costs to find, measured on a real answer.
///
/// A length-prefixed framing does not scan: it reads a header and then that
/// many bytes. NDJSON has to look at every byte until the delimiter, so this
/// is the framing tax, isolated from everything else.
#[derive(Serialize)]
struct Framing {
    delimiter_scan_us_p50: f64,
    delimiter_scan_us_max: f64,
    scanned_bytes: u64,
    share_of_p50_pct: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let steps = scenario(&args.workload)?;
    ensure_fixture(&args.fixture)?;

    if let Some(size) = args.pool {
        let contend = args.contend || args.writer_only;
        let report = pool::run(
            &args.fixture,
            size,
            contend,
            contend && !args.writer_only,
            args.samples,
            args.warmup,
        )
        .await?;
        if args.json {
            println!("{}", serde_json::to_string(&report)?);
        } else {
            pool::print_human(&report);
        }
        return Ok(());
    }

    let delivery = match args.delivery.as_deref() {
        Some(name) => name.parse::<Delivery>()?,
        None => steps[0].default_delivery(),
    };
    let handle_dir = args.handle_dir.clone().unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&handle_dir)
        .with_context(|| format!("creating {}", handle_dir.display()))?;

    let run_dir = std::env::temp_dir().join(format!("ipc-bench-{}", std::process::id()));
    let (listener, sock) = server::bind(&run_dir)?;

    // Two independent store handles, one per side, because a daemon and a
    // direct caller never share a connection either.
    let server_fixture = Fixture::open(&args.fixture)?;
    let local = Direct::new(Fixture::open(&args.fixture)?, args.direct_thread)?;
    let server =
        tokio::spawn(async move { server::serve_one(listener, server_fixture, handle_dir).await });

    let mut client = client::Client::connect(&sock).await?;

    for _ in 0..args.warmup {
        for step in &steps {
            client.sample(*step, delivery).await?;
        }
        local.run(&steps)?;
    }

    let mut serialize = Vec::with_capacity(args.samples);
    let mut frame_write = Vec::with_capacity(args.samples);
    let mut round_trip = Vec::with_capacity(args.samples);
    let mut dispatch = Vec::with_capacity(args.samples);
    let mut server_serialize = Vec::with_capacity(args.samples);
    let mut deserialize = Vec::with_capacity(args.samples);
    let mut total = Vec::with_capacity(args.samples);
    let mut direct = Vec::with_capacity(args.samples);
    let (mut bytes, mut frames) = (0u64, 0u32);
    let mut step_totals: Vec<Vec<f64>> = steps.iter().map(|_| Vec::with_capacity(args.samples)).collect();
    let mut step_bytes = vec![0u64; steps.len()];
    let mut step_frames = vec![0u32; steps.len()];
    let mut handle_read = Vec::with_capacity(args.samples);
    let mut handle_write = Vec::with_capacity(args.samples);
    let mut file_bytes = 0u64;

    for _ in 0..args.samples {
        // The pair is taken in one iteration so both sides see the same cache
        // state; `--direct-first` swaps which of them warms it for the other.
        let early = if args.direct_first { Some(local.run(&steps)?) } else { None };

        let mut agg = client::Sample::default();
        for (i, step) in steps.iter().enumerate() {
            let s = client.sample(*step, delivery).await?;
            step_totals[i].push(s.total);
            step_bytes[i] = s.bytes;
            step_frames[i] = s.frames;
            agg.serialize += s.serialize;
            agg.frame_write += s.frame_write;
            agg.round_trip += s.round_trip;
            agg.dispatch += s.dispatch;
            agg.server_serialize += s.server_serialize;
            agg.deserialize += s.deserialize;
            agg.handle_read += s.handle_read;
            agg.handle_write += s.handle_write;
            agg.handle_bytes += s.handle_bytes;
            agg.total += s.total;
            agg.bytes += s.bytes;
            agg.frames += s.frames;
        }
        handle_read.push(agg.handle_read);
        handle_write.push(agg.handle_write);
        file_bytes = agg.handle_bytes;
        serialize.push(agg.serialize);
        frame_write.push(agg.frame_write);
        round_trip.push(agg.round_trip);
        dispatch.push(agg.dispatch);
        server_serialize.push(agg.server_serialize);
        deserialize.push(agg.deserialize);
        total.push(agg.total);
        bytes = agg.bytes;
        frames = agg.frames;

        let elapsed = match early {
            Some(us) => us,
            None => local.run(&steps)?,
        };
        direct.push(elapsed);
    }

    let framing = probe_framing(client.last_frames(), &mut total.clone());
    drop(client); // closing the socket ends the server task
    server.await.context("joining the server task")??;
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_dir(&run_dir);

    let head = Stat::of(&mut total.clone());
    let mut stages = BTreeMap::new();
    stages.insert("serialize".to_string(), Stat::of(&mut serialize));
    stages.insert("frame_write".to_string(), Stat::of(&mut frame_write));
    stages.insert("round_trip".to_string(), Stat::of(&mut round_trip));
    stages.insert("dispatch".to_string(), Stat::of(&mut dispatch));
    stages.insert("server_serialize".to_string(), Stat::of(&mut server_serialize));
    stages.insert("deserialize".to_string(), Stat::of(&mut deserialize));
    stages.insert("handle_read".to_string(), Stat::of(&mut handle_read));
    stages.insert("handle_write".to_string(), Stat::of(&mut handle_write));
    stages.insert("direct".to_string(), Stat::of(&mut direct));

    let step_reports = steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let stat = Stat::of(&mut step_totals[i]);
            StepReport {
                method: step.as_str().to_string(),
                bytes: step_bytes[i],
                frames: step_frames[i],
                p50_us: stat.p50_us,
                p95_us: stat.p95_us,
            }
        })
        .collect();

    let moved = bytes + file_bytes;
    let report = Report {
        workload: args.workload.clone(),
        delivery: delivery.as_str(),
        samples: args.samples,
        p50_us: head.p50_us,
        p95_us: head.p95_us,
        max_us: head.max_us,
        stages,
        framing,
        bytes,
        frames,
        file_bytes,
        throughput_mbps: if head.p50_us > 0.0 {
            ((moved as f64 / head.p50_us) * 100.0).round() / 100.0
        } else {
            0.0
        },
        steps: step_reports,
        direct_mode: if args.direct_thread { "thread" } else { "inline" },
        order: if args.direct_first { "direct-first" } else { "rpc-first" },
    };

    if args.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print_human(&report);
    }
    Ok(())
}

/// Time finding the `\n` in every frame of one real answer, repeatedly.
fn probe_framing(frames: &[String], totals: &mut [f64]) -> Framing {
    let scanned: u64 = frames.iter().map(|f| f.len() as u64).sum();
    let mut runs: Vec<f64> = Vec::with_capacity(200);
    for _ in 0..200 {
        let t = Instant::now();
        let mut found = 0usize;
        for frame in frames {
            found += memchr::memchr(b'\n', frame.as_bytes()).unwrap_or(0);
        }
        std::hint::black_box(found);
        runs.push(t.elapsed().as_secs_f64() * 1e6);
    }
    let stat = Stat::of(&mut runs);
    let head = Stat::of(totals);
    Framing {
        delimiter_scan_us_p50: stat.p50_us,
        delimiter_scan_us_max: stat.max_us,
        scanned_bytes: scanned,
        share_of_p50_pct: if head.p50_us > 0.0 {
            ((stat.p50_us / head.p50_us) * 10000.0).round() / 100.0
        } else {
            0.0
        },
    }
}

/// The steps one scenario name expands to, in the order a client would make
/// them. `paged-sync` is what every sync event costs under paging; `paged`
/// adds the three interactions paging turns from in-memory list work into
/// server calls.
fn scenario(name: &str) -> Result<Vec<Workload>> {
    Ok(match name {
        "paged-sync" => vec![Workload::Page, Workload::Count],
        "paged" => vec![
            Workload::Page,
            Workload::Count,
            Workload::Jump,
            Workload::Filter,
            Workload::SelectAll,
        ],
        other => vec![other.parse()?],
    })
}

/// Where the direct half of the A/B runs.
///
/// `Inline` is the shape P1a-U1 measured: the direct call on the runtime's main
/// task, a few microseconds after the socket answer. `Thread` moves it to a
/// dedicated OS thread with its own store handle, which is the control for the
/// question of whether the `dispatch`-over-`direct` gap is a scheduling effect.
enum Direct {
    Inline(Fixture),
    Thread {
        send: std::sync::mpsc::Sender<Vec<Workload>>,
        recv: std::sync::mpsc::Receiver<Result<f64, String>>,
    },
}

impl Direct {
    fn new(fixture: Fixture, on_thread: bool) -> Result<Self> {
        if !on_thread {
            return Ok(Direct::Inline(fixture));
        }
        let (send, jobs) = std::sync::mpsc::channel::<Vec<Workload>>();
        let (answers, recv) = std::sync::mpsc::channel::<Result<f64, String>>();
        std::thread::Builder::new()
            .name("direct".into())
            .spawn(move || {
                while let Ok(steps) = jobs.recv() {
                    let answer = run_steps(&fixture, &steps).map_err(|e| e.to_string());
                    if answers.send(answer).is_err() {
                        return;
                    }
                }
            })
            .context("spawning the direct thread")?;
        Ok(Direct::Thread { send, recv })
    }

    /// Microseconds the direct calls took, timed on the thread that ran them.
    fn run(&self, steps: &[Workload]) -> Result<f64> {
        match self {
            Direct::Inline(fixture) => run_steps(fixture, steps),
            Direct::Thread { send, recv } => {
                send.send(steps.to_vec()).context("handing work to the direct thread")?;
                recv.recv()
                    .context("waiting on the direct thread")?
                    .map_err(|e| anyhow::anyhow!(e))
            }
        }
    }
}

fn run_steps(fixture: &Fixture, steps: &[Workload]) -> Result<f64> {
    let t = Instant::now();
    for step in steps {
        let produced = fixture.run(*step)?;
        std::hint::black_box(&produced);
    }
    Ok(t.elapsed().as_secs_f64() * 1e6)
}

fn print_human(r: &Report) {
    println!(
        "workload {} | delivery {} | {} samples | {} wire bytes + {} file bytes in {} frame(s) | {:.2} MB/s | direct {}, {}",
        r.workload,
        r.delivery,
        r.samples,
        r.bytes,
        r.file_bytes,
        r.frames,
        r.throughput_mbps,
        r.direct_mode,
        r.order
    );
    println!("round trip  p50 {:>10.2} us  p95 {:>10.2} us  max {:>10.2} us", r.p50_us, r.p95_us, r.max_us);
    println!("{:<18} {:>12} {:>12} {:>12}", "stage", "p50 us", "p95 us", "max us");
    for (name, s) in &r.stages {
        println!("{name:<18} {:>12.2} {:>12.2} {:>12.2}", s.p50_us, s.p95_us, s.max_us);
    }
    println!(
        "framing scan p50 {:.2} us over {} bytes ({:.2}% of the round trip p50)",
        r.framing.delimiter_scan_us_p50, r.framing.scanned_bytes, r.framing.share_of_p50_pct
    );
    if r.steps.len() > 1 {
        println!("{:<12} {:>10} {:>8} {:>12} {:>12}", "step", "bytes", "frames", "p50 us", "p95 us");
        for s in &r.steps {
            println!(
                "{:<12} {:>10} {:>8} {:>12.2} {:>12.2}",
                s.method, s.bytes, s.frames, s.p50_us, s.p95_us
            );
        }
    }
}

/// Build the fixture when the directory is not there, with the exact command
/// `docs/baselines/pre-daemon/workloads.md` documents.
fn ensure_fixture(root: &std::path::Path) -> Result<()> {
    if root.join("data").join("accounts").join("alpha").join("store.sqlite3").exists() {
        return Ok(());
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("Cargo.toml");
    eprintln!("building the fixture in {} (this takes a minute)", root.display());
    let status = Command::new("cargo")
        .arg("run")
        .arg("--release")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--example")
        .arg("mkfixture")
        .arg("--")
        .arg("--out")
        .arg(root)
        .arg("--rows")
        .arg("5000")
        .status()
        .context("running mkfixture")?;
    if !status.success() {
        anyhow::bail!("mkfixture failed with {status}");
    }
    Ok(())
}
