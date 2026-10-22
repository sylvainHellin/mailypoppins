//! The release workflow's prerelease rule (#0139, stage 2): a tag with a
//! SemVer pre-release part (`v0.12.0-rc.1`) is published as a GitHub
//! prerelease and never becomes the latest release, so installed apps,
//! which read `releases/latest/download/latest.json`, and the Homebrew tap
//! are never offered it.
//!
//! The steps' own `run:` scripts are read out of
//! `.github/workflows/release.yml` and run under bash with a stand-in `gh` on
//! `PATH` that records its arguments, so the test checks the shipped shell,
//! not a copy of it.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_yaml::Value;

fn workflow() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml");
    let text = std::fs::read_to_string(&path).expect("release.yml");
    serde_yaml::from_str(&text).expect("release.yml parses")
}

fn job<'a>(workflow: &'a Value, name: &str) -> &'a Value {
    &workflow["jobs"][name]
}

/// The `run:` of the job's step whose name starts with `step`.
fn step_script(workflow: &Value, job_name: &str, step: &str) -> String {
    job(workflow, job_name)["steps"]
        .as_sequence()
        .expect("steps")
        .iter()
        .find(|s| s["name"].as_str().is_some_and(|n| n.starts_with(step)))
        .and_then(|s| s["run"].as_str())
        .unwrap_or_else(|| panic!("no step {step:?} with a run in {job_name}"))
        .to_string()
}

/// Run `script` as GitHub runs a `run:` (bash, `-e`, `pipefail`) for the tag
/// `tag`, with a `gh` that only records its arguments; answers what `gh` was
/// called with, one line per call.
fn run_for_tag(script: &str, tag: &str) -> Vec<String> {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let log = dir.path().join("gh.log");
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        format!("#!/bin/sh\necho \"$*\" >> '{}'\n", log.display()),
    )
    .expect("gh");
    std::fs::set_permissions(
        &gh,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("chmod");
    let path: PathBuf = bin;
    let joined = std::env::join_paths(std::iter::once(path).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .expect("PATH");
    let out = Command::new("bash")
        .args(["--noprofile", "--norc", "-eo", "pipefail", "-c", script])
        .current_dir(dir.path())
        .env("PATH", joined)
        .env("GITHUB_REF_NAME", tag)
        .env("GITHUB_REPOSITORY", "sylvainHellin/mailypoppins")
        .env("GH_TOKEN", "test")
        .output()
        .expect("bash");
    assert!(
        out.status.success(),
        "the step failed for {tag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_prerelease_tag_is_created_as_a_prerelease_and_a_release_tag_is_not() {
    let wf = workflow();
    let script = step_script(&wf, "create-release", "Create GitHub release");

    let pre = run_for_tag(&script, "v0.12.0-rc.1");
    assert_eq!(pre.len(), 1, "{pre:?}");
    assert!(
        pre[0].starts_with("release create v0.12.0-rc.1 "),
        "{pre:?}"
    );
    assert!(pre[0].contains("--prerelease=true"), "{pre:?}");
    assert!(pre[0].contains("--latest=false"), "{pre:?}");

    let release = run_for_tag(&script, "v0.12.0");
    assert_eq!(release.len(), 1, "{release:?}");
    assert!(release[0].contains("--prerelease=false"), "{release:?}");
    assert!(release[0].contains("--latest=false"), "{release:?}");
}

#[test]
fn only_a_release_tag_is_marked_latest_after_its_manifest() {
    let wf = workflow();
    let script = step_script(&wf, "desktop-manifest", "Mark the release latest");

    assert_eq!(run_for_tag(&script, "v0.12.0-rc.1"), Vec::<String>::new());
    assert_eq!(
        run_for_tag(&script, "v0.12.0"),
        vec!["release edit v0.12.0 --repo sylvainHellin/mailypoppins --latest".to_string()]
    );
}

/// Evaluate a job's `if:` for the tag `ref_name`, for the one expression form
/// the workflow uses: terms `contains(github.ref_name, '<literal>')`, each
/// optionally negated with `!`, joined by `&&`, optionally wrapped in
/// `${{ }}`. Whitespace and the quote style do not matter; any other form
/// panics, so a rewrite the matcher cannot read fails the test instead of
/// passing it by accident.
fn eval_ref_condition(condition: &str, ref_name: &str) -> bool {
    let mut expr = condition.trim();
    if let Some(inner) = expr.strip_prefix("${{").and_then(|e| e.strip_suffix("}}")) {
        expr = inner.trim();
    }
    expr.split("&&").all(|term| {
        let mut term = term.trim();
        let negated = term.starts_with('!');
        if negated {
            term = term[1..].trim_start();
        }
        let args = term
            .strip_prefix("contains")
            .map(str::trim_start)
            .and_then(|t| t.strip_prefix('('))
            .and_then(|t| t.strip_suffix(')'))
            .unwrap_or_else(|| panic!("unsupported term {term:?} in {condition:?}"));
        let (subject, needle) = args
            .split_once(',')
            .unwrap_or_else(|| panic!("contains() needs two arguments in {condition:?}"));
        assert_eq!(subject.trim(), "github.ref_name", "{condition:?}");
        let needle = needle.trim();
        let literal = needle
            .strip_prefix('\'')
            .and_then(|n| n.strip_suffix('\''))
            .or_else(|| needle.strip_prefix('"').and_then(|n| n.strip_suffix('"')))
            .unwrap_or_else(|| panic!("unquoted literal {needle:?} in {condition:?}"));
        ref_name.contains(literal) != negated
    })
}

#[test]
fn the_ref_condition_matcher_reads_reformatted_conditions() {
    for form in [
        "${{ !contains(github.ref_name, '-') }}",
        "!contains(github.ref_name,'-')",
        "${{!contains( github.ref_name , \"-\" )}}",
        "${{ ! contains (github.ref_name, '-') }}",
    ] {
        assert!(eval_ref_condition(form, "v0.12.0"), "{form}");
        assert!(!eval_ref_condition(form, "v0.12.0-rc.1"), "{form}");
    }
    assert!(eval_ref_condition(
        "contains(github.ref_name, '-')",
        "v0.12.0-rc.1"
    ));
}

#[test]
fn the_homebrew_tap_skips_a_prerelease_tag() {
    let wf = workflow();
    let condition = job(&wf, "homebrew-tap")["if"]
        .as_str()
        .expect("homebrew-tap has a job-level if");
    assert!(eval_ref_condition(condition, "v0.12.0"), "{condition}");
    assert!(
        !eval_ref_condition(condition, "v0.12.0-rc.1"),
        "{condition}"
    );
}
