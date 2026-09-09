//! P1a-U1: what a JSON-RPC-over-Unix-socket round trip costs against the same
//! work done as a direct library call.
//!
//! Both sides read the same fixture store through the same product API
//! (`mailypoppins::store::read`), so every microsecond of difference is
//! transport: request encode, framing, socket, dispatch, response decode.
//! Run recipe and output shape: see README.md.

mod client;
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

use crate::stats::Stat;
use crate::work::{Fixture, Workload};

#[derive(Parser)]
#[command(about = "Measure NDJSON JSON-RPC over a Unix socket against a direct library call")]
struct Args {
    /// Workload to run: w1 (preview), w2 (5000-row list), w3 (whole-account
    /// stream), w4 (10 MiB body).
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
}

/// The contract shape: the top-level figures are the client-observed round
/// trip, `stages` breaks them down and carries the direct call beside them.
#[derive(Serialize)]
struct Report {
    workload: String,
    samples: usize,
    p50_us: f64,
    p95_us: f64,
    max_us: f64,
    stages: BTreeMap<String, Stat>,
    framing: Framing,
    /// Payload bytes of one answer, delimiters excluded.
    bytes: u64,
    /// Frames one answer took, response frame included.
    frames: u32,
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
    let workload: Workload = args.workload.parse()?;
    ensure_fixture(&args.fixture)?;

    let run_dir = std::env::temp_dir().join(format!("ipc-bench-{}", std::process::id()));
    let (listener, sock) = server::bind(&run_dir)?;

    // Two independent store handles, one per side, because a daemon and a
    // direct caller never share a connection either.
    let server_fixture = Fixture::open(&args.fixture)?;
    let local = Fixture::open(&args.fixture)?;
    let server = tokio::spawn(async move { server::serve_one(listener, server_fixture).await });

    let mut client = client::Client::connect(&sock).await?;

    for _ in 0..args.warmup {
        client.sample(workload).await?;
        local.run(workload)?;
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

    for _ in 0..args.samples {
        let s = client.sample(workload).await?;
        serialize.push(s.serialize);
        frame_write.push(s.frame_write);
        round_trip.push(s.round_trip);
        dispatch.push(s.dispatch);
        server_serialize.push(s.server_serialize);
        deserialize.push(s.deserialize);
        total.push(s.total);
        bytes = s.bytes;
        frames = s.frames;

        let t = Instant::now();
        let produced = local.run(workload)?;
        direct.push(t.elapsed().as_secs_f64() * 1e6);
        std::hint::black_box(&produced);
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
    stages.insert("direct".to_string(), Stat::of(&mut direct));

    let report = Report {
        workload: workload.as_str().to_string(),
        samples: args.samples,
        p50_us: head.p50_us,
        p95_us: head.p95_us,
        max_us: head.max_us,
        stages,
        framing,
        bytes,
        frames,
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

fn print_human(r: &Report) {
    println!("workload {} | {} samples | {} bytes in {} frame(s)", r.workload, r.samples, r.bytes, r.frames);
    println!("round trip  p50 {:>10.2} us  p95 {:>10.2} us  max {:>10.2} us", r.p50_us, r.p95_us, r.max_us);
    println!("{:<18} {:>12} {:>12} {:>12}", "stage", "p50 us", "p95 us", "max us");
    for (name, s) in &r.stages {
        println!("{name:<18} {:>12.2} {:>12.2} {:>12.2}", s.p50_us, s.p95_us, s.max_us);
    }
    println!(
        "framing scan p50 {:.2} us over {} bytes ({:.2}% of the round trip p50)",
        r.framing.delimiter_scan_us_p50, r.framing.scanned_bytes, r.framing.share_of_p50_pct
    );
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
