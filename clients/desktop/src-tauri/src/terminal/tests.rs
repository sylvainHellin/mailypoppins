use super::*;
use std::collections::{BTreeMap, VecDeque};

// ---------------------------------------------------------------------------
// Coalescing
// ---------------------------------------------------------------------------

#[test]
fn small_chunks_inside_the_window_make_one_frame() {
    let t0 = Instant::now();
    let mut c = Coalescer::new(LIMITS);
    for i in 0..50u64 {
        let at = t0 + Duration::from_micros(i * 50);
        assert!(c.push(b"0123456789", at).is_empty());
        assert_eq!(c.poll(at), None, "not due yet");
    }
    assert_eq!(
        c.deadline(),
        Some(t0 + FRAME_WINDOW),
        "the window counts from the first byte"
    );
    let frame = c.poll(t0 + FRAME_WINDOW).expect("due");
    assert_eq!(frame.len(), 500);
    assert!(c.is_empty());
    assert_eq!(c.deadline(), None);
}

#[test]
fn a_gap_longer_than_the_window_makes_two_frames() {
    let t0 = Instant::now();
    let mut c = Coalescer::new(LIMITS);
    c.push(b"one", t0);
    let first = c.poll(t0 + FRAME_WINDOW + Duration::from_millis(1));
    assert_eq!(first.as_deref(), Some(&b"one"[..]));
    let t1 = t0 + Duration::from_millis(10);
    c.push(b"two", t1);
    assert_eq!(c.deadline(), Some(t1 + FRAME_WINDOW), "a new window");
    assert_eq!(c.flush().as_deref(), Some(&b"two"[..]));
    assert_eq!(c.flush(), None);
}

#[test]
fn a_frame_is_cut_at_64_kib() {
    let t0 = Instant::now();
    let mut c = Coalescer::new(LIMITS);
    assert!(c.push(&vec![b'a'; FRAME_MAX - 1], t0).is_empty());
    let full = c.push(b"bc", t0);
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].len(), FRAME_MAX);
    assert_eq!(full[0][FRAME_MAX - 1], b'b');
    assert_eq!(c.flush().as_deref(), Some(&b"c"[..]));

    let full = c.push(&vec![b'x'; 2 * FRAME_MAX + 7], t0);
    assert_eq!(
        full.iter().map(Vec::len).collect::<Vec<_>>(),
        [FRAME_MAX, FRAME_MAX]
    );
    assert_eq!(c.flush().map(|f| f.len()), Some(7));

    assert_eq!(c.push(&vec![b'y'; FRAME_MAX], t0).len(), 1);
    assert!(c.is_empty(), "exactly 64 KiB leaves nothing pending");
    assert_eq!(c.deadline(), None);
}

/// A reader that answers one scripted chunk per read, after its delay.
struct Script {
    steps: VecDeque<(Duration, Vec<u8>)>,
    done: Arc<AtomicBool>,
}

impl Read for Script {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.steps.pop_front() {
            Some((delay, bytes)) => {
                std::thread::sleep(delay);
                buf[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            None => {
                self.done.store(true, Ordering::SeqCst);
                Ok(0)
            }
        }
    }
}

/// A child that exits with `code` once the script ran out.
struct Scripted {
    done: Arc<AtomicBool>,
    code: i32,
}

impl Reap for Scripted {
    fn try_reap(&mut self) -> Option<TerminalExit> {
        self.done.load(Ordering::SeqCst).then_some(TerminalExit {
            code: Some(self.code),
            signal: None,
        })
    }

    fn kill(&mut self) {}
}

fn run_script(steps: Vec<(Duration, Vec<u8>)>, limits: Limits) -> Vec<Frame> {
    let done = Arc::new(AtomicBool::new(false));
    let script = Script {
        steps: steps.into(),
        done: done.clone(),
    };
    let rx = spawn_reader(Box::new(script));
    let mut frames = Vec::new();
    pump(rx, limits, &mut Scripted { done, code: 3 }, &mut |f| {
        frames.push(f)
    });
    frames
}

#[test]
fn the_pump_coalesces_a_scripted_reader_and_sends_the_exit_last() {
    // A wide window, so a busy test machine cannot split the burst.
    let wide = Limits {
        max: FRAME_MAX,
        window: Duration::from_millis(300),
    };
    let burst: Vec<_> = (0..40).map(|_| (Duration::ZERO, b"abc".to_vec())).collect();
    let frames = run_script(burst, wide);
    assert_eq!(
        frames,
        [
            Frame::Output(b"abc".repeat(40)),
            Frame::Exit(TerminalExit {
                code: Some(3),
                signal: None
            })
        ]
    );

    // The production window, with a gap far beyond it.
    let gap = vec![
        (Duration::ZERO, b"one".to_vec()),
        (Duration::from_millis(60), b"two".to_vec()),
    ];
    let frames = run_script(gap, LIMITS);
    assert_eq!(frames.len(), 3, "{frames:?}");
    assert_eq!(frames[0], Frame::Output(b"one".to_vec()));
    assert_eq!(frames[1], Frame::Output(b"two".to_vec()));
    assert!(matches!(frames[2], Frame::Exit(_)));

    // 64 KiB arriving fast is cut there, whatever the window.
    let big = vec![
        (Duration::ZERO, vec![b'z'; FRAME_MAX]),
        (Duration::ZERO, b"!".to_vec()),
    ];
    let frames = run_script(big, wide);
    let sizes: Vec<usize> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Output(b) => Some(b.len()),
            Frame::Exit(_) => None,
        })
        .collect();
    assert_eq!(sizes, [FRAME_MAX, 1]);
}

#[test]
fn the_exit_frame_is_the_contracts_json() {
    let body = Frame::Exit(TerminalExit {
        code: Some(0),
        signal: None,
    })
    .body();
    match body {
        InvokeResponseBody::Json(json) => {
            assert_eq!(json, r#"{"exit":{"code":0,"signal":null}}"#)
        }
        other => panic!("expected JSON, got {other:?}"),
    }
    let body = Frame::Output(vec![1, 2]).body();
    assert!(matches!(body, InvokeResponseBody::Raw(b) if b == [1, 2]));
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |name| map.get(name).cloned()
}

fn files_at(paths: &'static [&'static str]) -> impl Fn(&Path) -> bool {
    move |p| paths.iter().any(|f| p == Path::new(f))
}

fn lookup<'a>(
    env: &'a dyn Fn(&str) -> Option<String>,
    setting: Option<&str>,
    is_file: &'a dyn Fn(&Path) -> bool,
) -> Lookup<'a> {
    Lookup {
        env,
        setting: setting.map(str::to_string),
        is_file,
        macos: true,
    }
}

fn draft(tag: &str) -> String {
    let dir = crate::test_support::scratch_dir(tag);
    let path = dir.join("d.md");
    std::fs::write(&path, "---\nstatus: draft\n---\n").expect("draft");
    path.to_string_lossy().into_owned()
}

fn setup_message<T: std::fmt::Debug>(r: Result<T, GuiError>) -> String {
    match r {
        Err(GuiError::Setup { message }) => message,
        other => panic!("expected a setup error, got {other:?}"),
    }
}

#[test]
fn a_terminal_editor_in_the_environment_runs_bare_never_wrapped() {
    let path = draft("term-env");
    let files = files_at(&[
        "/opt/homebrew/bin/nvim",
        "/Applications/Ghostty.app/Contents/MacOS/ghostty",
    ]);
    let env = env_of(&[("EDITOR", "nvim"), ("HOME", "/home/u")]);
    let l = lookup(&env, None, &files);
    let wrapped = editor::resolve(&l);
    assert_eq!(wrapped.source, EditorSource::Terminal);
    assert!(
        wrapped.template.starts_with("open -na"),
        "{}",
        wrapped.template
    );

    let launch = plan(&l, "/usr/bin:/bin", &path, false).expect("plan");
    assert_eq!(launch.argv, ["/opt/homebrew/bin/nvim", path.as_str()]);
    assert_eq!(launch.source, EditorSource::Editor);
    assert_eq!(launch.cwd, Path::new(&path).parent().expect("dir"));
    assert_eq!(
        launch.env,
        [
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("COLORTERM".to_string(), "truecolor".to_string()),
        ]
    );
}

#[test]
fn the_explicit_choice_wins_and_a_terminal_editor_beats_a_gui_visual() {
    let path = draft("term-order");
    let files = files_at(&["/x/nvim", "/x/hx", "/x/vim"]);
    let env = env_of(&[
        (EDITOR_ENV, "vim -u NONE"),
        ("VISUAL", "code -w"),
        ("EDITOR", "hx"),
    ]);
    let launch = plan(&lookup(&env, Some("nvim"), &files), "/x", &path, false).expect("plan");
    assert_eq!(launch.argv, ["/x/vim", "-u", "NONE", path.as_str()]);
    assert_eq!(launch.source, EditorSource::Env);

    let env = env_of(&[("VISUAL", "code -w"), ("EDITOR", "hx")]);
    let launch = plan(
        &lookup(&env, Some("nvim {path} +1"), &files),
        "/x",
        &path,
        false,
    )
    .expect("plan");
    assert_eq!(launch.argv, ["/x/nvim", path.as_str(), "+1"]);
    assert_eq!(launch.source, EditorSource::Setting);

    let launch = plan(&lookup(&env, None, &files), "/x", &path, false).expect("plan");
    assert_eq!(launch.argv, ["/x/hx", path.as_str()]);
    assert_eq!(launch.source, EditorSource::Editor);
}

#[test]
fn a_gui_editor_is_a_setup_error_naming_it() {
    let path = draft("term-gui");
    let files = files_at(&["/x/nvim", "/usr/local/bin/code"]);
    let env = env_of(&[("EDITOR", "code -w")]);
    let message = setup_message(plan(&lookup(&env, None, &files), "/x", &path, false));
    assert!(message.contains("`code -w`"), "{message}");
    assert!(message.contains("$EDITOR"), "{message}");

    let env = env_of(&[]);
    let message = setup_message(plan(&lookup(&env, Some("zed"), &files), "/x", &path, false));
    assert!(
        message.contains("the editor setting names `zed`"),
        "{message}"
    );
    // The fixture refuses it too, so the UI's refusal can be exercised.
    assert!(matches!(
        plan(&lookup(&env, Some("zed"), &files), "/x", &path, true),
        Err(GuiError::Setup { .. })
    ));
}

#[test]
fn with_nothing_named_nvim_then_vim_then_hx_are_probed() {
    let path = draft("term-probe");
    let env = env_of(&[("HOME", "/home/u")]);
    let run = |files: &'static [&'static str]| {
        let is = files_at(files);
        plan(&lookup(&env, None, &is), "/x:/y", &path, false).map(|l| (l.argv[0].clone(), l.source))
    };
    assert_eq!(
        run(&[
            "/x/hx",
            "/usr/bin/vim",
            "/home/u/.local/share/bob/nvim-bin/nvim"
        ])
        .expect("bob"),
        (
            "/home/u/.local/share/bob/nvim-bin/nvim".to_string(),
            EditorSource::Probe
        ),
        "name by name: a Neovim anywhere beats a vim or an hx"
    );
    assert_eq!(
        run(&["/opt/homebrew/bin/nvim", "/y/nvim"])
            .expect("login path")
            .0,
        "/y/nvim",
        "the login PATH before the probe directories"
    );
    assert_eq!(
        run(&[
            "/opt/homebrew/bin/nvim",
            "/home/u/.local/share/bob/nvim-bin/nvim"
        ])
        .expect("homebrew")
        .0,
        "/opt/homebrew/bin/nvim",
        "the probe directories before bob"
    );
    assert_eq!(
        run(&["/x/hx", "/usr/bin/vim"]).expect("vim").0,
        "/usr/bin/vim"
    );
    let message = setup_message(run(&[]));
    assert!(message.contains("nvim, vim, hx"), "{message}");
    assert!(message.contains("bob"), "{message}");

    let none = |_: &Path| false;
    let launch = plan(&lookup(&env, None, &none), "/x", &path, true).expect("fixture");
    assert_eq!(
        launch.argv,
        ["nvim", path.as_str()],
        "the fixture journals a bare nvim"
    );
}

#[test]
fn a_named_editor_found_nowhere_is_a_setup_error() {
    let path = draft("term-missing");
    let none = |_: &Path| false;
    let env = env_of(&[("EDITOR", "nvim")]);
    let message = setup_message(plan(&lookup(&env, None, &none), "/x", &path, false));
    assert!(
        message.contains("`nvim` from $EDITOR was not found"),
        "{message}"
    );
    assert!(message.contains("full path"), "{message}");
    let message = setup_message(plan(
        &lookup(&env, Some("/no/such/nvim"), &none),
        "/x",
        &path,
        false,
    ));
    assert!(message.contains("/no/such/nvim"), "{message}");
    assert!(matches!(
        plan(&lookup(&env, None, &none), "/x", "relative.md", false),
        Err(GuiError::Protocol { .. })
    ));
    assert!(matches!(
        plan(&lookup(&env, None, &none), "/x", "/no/such/draft.md", false),
        Err(GuiError::NotFound { .. })
    ));
}

#[test]
fn the_login_path_is_the_last_line_the_shell_printed() {
    assert_eq!(
        parse_login_path(b"welcome!\n/opt/homebrew/bin:/usr/bin").as_deref(),
        Some("/opt/homebrew/bin:/usr/bin")
    );
    assert_eq!(parse_login_path(b"/a:/b\n").as_deref(), Some("/a:/b"));
    assert_eq!(parse_login_path(b"no path here"), None);
    assert_eq!(parse_login_path(b""), None);
    let path = login_shell_path("/bin/sh").expect("sh answers");
    assert!(path.contains('/'), "{path}");
    assert!(login_shell_path("/no/such/shell").is_err());
}

// ---------------------------------------------------------------------------
// A real PTY
// ---------------------------------------------------------------------------

/// The frames a channel received, decoded.
#[derive(Clone, Default)]
struct Received(Arc<Mutex<Vec<Frame>>>);

impl Received {
    fn channel(&self) -> Channel<InvokeResponseBody> {
        let frames = self.0.clone();
        Channel::new(move |body| {
            let frame = match body {
                InvokeResponseBody::Raw(bytes) => Frame::Output(bytes),
                InvokeResponseBody::Json(json) => {
                    let v: serde_json::Value = serde_json::from_str(&json).expect("json");
                    let n = |k: &str| v["exit"][k].as_i64().map(|n| n as i32);
                    Frame::Exit(TerminalExit {
                        code: n("code"),
                        signal: n("signal"),
                    })
                }
            };
            lock(&frames).push(frame);
            Ok(())
        })
    }

    fn frames(&self) -> Vec<Frame> {
        lock(&self.0).clone()
    }

    fn text(&self) -> String {
        let bytes: Vec<u8> = self
            .frames()
            .into_iter()
            .filter_map(|f| match f {
                Frame::Output(b) => Some(b),
                Frame::Exit(_) => None,
            })
            .flatten()
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn exit(&self) -> Option<TerminalExit> {
        self.frames().into_iter().find_map(|f| match f {
            Frame::Exit(e) => Some(e),
            Frame::Output(_) => None,
        })
    }

    fn wait_for(&self, what: &str, until: impl Fn(&Received) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !until(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; got {:?}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_text(&self, needle: &str) {
        self.wait_for(needle, |r| r.text().contains(needle));
    }

    fn wait_exit(&self) -> TerminalExit {
        self.wait_for("the exit frame", |r| r.exit().is_some());
        let frames = self.frames();
        assert!(
            matches!(frames.last(), Some(Frame::Exit(_))),
            "the exit is the last frame: {frames:?}"
        );
        assert_eq!(
            frames
                .iter()
                .filter(|f| matches!(f, Frame::Exit(_)))
                .count(),
            1
        );
        self.exit().expect("exit")
    }
}

fn sh(script: &str, tag: &str) -> (Draft, Launch) {
    let path = draft(tag);
    let launch = Launch {
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        cwd: Path::new(&path).parent().expect("dir").to_path_buf(),
        env: vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
        ],
        source: EditorSource::Probe,
    };
    let draft = Draft {
        account: "work".into(),
        id: "d".into(),
        path,
    };
    (draft, launch)
}

#[test]
fn a_real_pty_echoes_resizes_and_exits_with_its_status() {
    let script = r#"printf 'size:%s\n' "$(stty size)"; printf 'env:%s:%s:%s\n' "$TERM" "$COLORTERM" "$(pwd -P)"
while IFS= read -r l; do
  case "$l" in
    size) printf 'size:%s\n' "$(stty size)" ;;
    quit) echo bye; exit 0 ;;
    *) echo "got:$l" ;;
  esac
done"#;
    let (d, launch) = sh(script, "pty-echo");
    let cwd = std::fs::canonicalize(&launch.cwd).expect("cwd");
    let terminals = Terminals::default();
    let rx = Received::default();
    let started = terminals
        .start(None, d, launch, 80, 24, rx.channel())
        .expect("spawn");
    assert!(!started.fixture);
    assert!(started.pid.is_some());
    assert!(
        started.editor.starts_with("/bin/sh -c "),
        "{}",
        started.editor
    );

    rx.wait_text("size:24 80");
    rx.wait_text(&format!("env:xterm-256color:truecolor:{}", cwd.display()));
    terminals.write(started.session, "hello\r").expect("write");
    rx.wait_text("got:hello");
    terminals.resize(started.session, 100, 30).expect("resize");
    terminals.write(started.session, "size\r").expect("write");
    rx.wait_text("size:30 100");
    terminals.write(started.session, "quit\r").expect("write");
    let exit = rx.wait_exit();
    assert_eq!(
        exit,
        TerminalExit {
            code: Some(0),
            signal: None
        }
    );
    assert!(rx.text().contains("bye"));

    // Writes and resizes to an exited session are dropped, a kill drops it,
    // and a second kill is fine.
    terminals.write(started.session, "late\r").expect("dropped");
    terminals.resize(started.session, 90, 20).expect("dropped");
    terminals.kill(started.session);
    terminals.kill(started.session);
    assert!(matches!(
        terminals.write(started.session, "x"),
        Err(GuiError::NotFound { .. })
    ));
    assert_eq!(
        rx.frames()
            .iter()
            .filter(|f| matches!(f, Frame::Exit(_)))
            .count(),
        1
    );
}

#[test]
fn a_nonzero_exit_is_reported() {
    let (d, launch) = sh("echo going; exit 7", "pty-seven");
    let terminals = Terminals::default();
    let rx = Received::default();
    terminals
        .start(None, d, launch, 80, 24, rx.channel())
        .expect("spawn");
    assert_eq!(
        rx.wait_exit(),
        TerminalExit {
            code: Some(7),
            signal: None
        }
    );
    assert!(rx.text().contains("going"));
}

#[cfg(unix)]
#[test]
fn a_kill_ends_the_child_with_a_signal_and_sends_the_exit_before_it_returns() {
    let (d, launch) = sh("echo ready; exec cat", "pty-kill");
    let terminals = Terminals::default();
    let rx = Received::default();
    let started = terminals
        .start(None, d, launch, 80, 24, rx.channel())
        .expect("spawn");
    rx.wait_text("ready");
    terminals.kill(started.session);
    let exit = rx.exit().expect("the exit frame went before kill returned");
    assert_eq!(
        exit,
        TerminalExit {
            code: None,
            signal: Some(1)
        },
        "SIGHUP"
    );
    assert!(matches!(rx.frames().last(), Some(Frame::Exit(_))));

    // A child that ignores SIGHUP gets SIGKILL.
    let (d, launch) = sh(
        "trap '' HUP; echo ready; while :; do sleep 0.05; done",
        "pty-kill9",
    );
    let rx = Received::default();
    let started = terminals
        .start(None, d, launch, 80, 24, rx.channel())
        .expect("spawn");
    rx.wait_text("ready");
    terminals.kill(started.session);
    assert_eq!(
        rx.exit(),
        Some(TerminalExit {
            code: None,
            signal: Some(9)
        }),
        "SIGKILL"
    );
}

#[cfg(unix)]
#[test]
fn dropping_the_table_kills_every_live_child() {
    let (d, launch) = sh("echo ready; exec cat", "pty-drop");
    let terminals = Terminals::default();
    let rx = Received::default();
    terminals
        .start(None, d, launch, 80, 24, rx.channel())
        .expect("spawn");
    rx.wait_text("ready");
    drop(terminals);
    assert_eq!(rx.wait_exit().signal, Some(1));
}

#[test]
fn a_fixture_session_journals_spawns_nothing_and_exits_on_kill() {
    let (tx, _events) = std::sync::mpsc::channel();
    let f = Arc::new(Fixture::load(tx).expect("fixture"));
    let door = crate::session::Door::Fixture(f.clone());
    let at = crate::commands::draft_path_on(&door, "work", "offsite-note").expect("path");
    let env = env_of(&[("EDITOR", "nvim -u NONE")]);
    let none = |_: &Path| false;
    let launch = plan(&lookup(&env, None, &none), "/x", &at.path, true).expect("plan");
    let terminals = Terminals::default();
    let rx = Received::default();
    let draft = Draft {
        account: "work".into(),
        id: "offsite-note".into(),
        path: at.path.clone(),
    };
    let started = terminals
        .start(Some(&f), draft, launch, 80, 24, rx.channel())
        .expect("fixture start");
    assert_eq!(started.pid, None);
    assert!(started.fixture);
    assert_eq!(started.source, EditorSource::Editor);
    assert_eq!(started.editor, format!("nvim -u NONE {}", quote(&at.path)));
    let opens = f.editor_opens();
    assert_eq!(opens.len(), 1);
    assert_eq!(opens[0].command, ["nvim", "-u", "NONE", at.path.as_str()]);
    f.simulate("editor_save")
        .expect("editor_save plays against it");

    terminals.write(started.session, "ignored").expect("write");
    terminals.resize(started.session, 100, 30).expect("resize");
    assert!(rx.frames().is_empty(), "no frames before the kill");
    terminals.kill(started.session);
    assert_eq!(
        rx.frames(),
        [Frame::Exit(TerminalExit {
            code: Some(0),
            signal: None
        })]
    );
    terminals.kill(started.session);
    assert_eq!(rx.frames().len(), 1, "a second kill sends nothing");
}
