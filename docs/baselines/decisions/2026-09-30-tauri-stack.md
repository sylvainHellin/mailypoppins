# Research: Tauri 2 + React stack for a Rust email client (desktop spike)

Date of research: 2026-09-29. All versions verified live against crates.io / npm.

## 1. Current Tauri 2 versions, cadence, 3.0 horizon

**Verdict: pin Tauri 2.12.x; 3.0 is in alpha and not a reason to wait.**

- `tauri` crate: stable 2.12.0 (published 2026-09-26), MSRV rust_version = 1.90. `tauri-cli`: 2.12.0 stable (same day). [crates.io](https://crates.io/crates/tauri), [crates.io](https://crates.io/crates/tauri-cli)
- npm: `@tauri-apps/api` 2.12.0, `@tauri-apps/cli` 2.12.0. [npm](https://www.npmjs.com/package/@tauri-apps/api)
- Cadence: frequent patch/minor releases, roughly every 2–8 weeks per crate (2.11.5 was Jul 1 2026; 2.12.0 Sep 26 2026). Running changelog: [Tauri Core Releases](https://tauri.app/release/core/)
- Tauri 3.0: `3.0.0-alpha.3` shipped 2026-09-26 (alpha.0 was mid-Sep 2026); the GitHub 3.0 milestone is ~25% complete, no due date. Alpha breaking changes so far include: `tauri-build` no longer copies resources to the cargo target dir (resolved at runtime in dev), `run_on_main_thread` removed in favour of the `Manager` trait, `objc-exception` feature flag removed. MSRV for 3.0 alphas is 1.95. [Release 3.0.0-alpha.3](https://github.com/tauri-apps/tauri/releases/tag/tauri-v3.0.0-alpha.3), [milestone](https://github.com/tauri-apps/tauri/milestone/5)
- Minimum macOS: docs say apps support macOS 10.13+ by default ([bundle docs](https://v2.tauri.app/distribute/macos-application-bundle/), Jul 2025), but Tauri 2.11+ moved to objc2 WebKit APIs unavailable on Catalina — Catalina is no longer supported, effectively macOS 11+ ([issue #15431](https://github.com/tauri-apps/tauri/issues/15431), May 2026; [homebrew PR](https://github.com/thedavidweng/homebrew-tap/pull/18)). macOS 15 on Apple Silicon is far above either bar.
- `create-tauri-app` scaffolds React + TypeScript on Vite; the wizard supports pnpm (also npm/yarn/bun) and writes `src-tauri/tauri.conf.json`. [Create a Project](https://v2.tauri.app/start/create-project/)

Spike: `pnpm create tauri-app@latest -m pnpm -t react-ts` (Vite template), then `tauri` = "2.12", plugins "*" resolved by cargo/npm.

## 2. Capabilities/permissions and CSP

**Verdict: capabilities model is straightforward; set an explicit CSP; sandboxed `srcdoc` iframes inherit the parent CSP.**

- Permissions are declared per-plugin as strings inside capability files under `src-tauri/capabilities/`. The scaffolded default is `src-tauri/capabilities/default.json` with `identifier`, `windows: ["main"]`, `permissions: ["core:path:default", "core:window:default", ...]`. Plugin permissions are added as e.g. `"shell:allow-open"`. [Capabilities docs](https://v2.tauri.app/security/capabilities/)
- `dangerousRemoteDomainIpcAccess` (v1) is gone; remote API access is now expressed via a capability's `remote` URLs field. For this app it's irrelevant: your email frames are sandboxed local documents with no Tauri IPC — do not grant them capabilities. [Capabilities docs](https://v2.tauri.app/security/capabilities/)
- CSP is opt-in via `tauri.conf.json > security.csp` (string or per-directive map, e.g. `connect-src: "ipc: http://ipc.localhost"`). At build time Tauri appends nonces/hashes for bundled assets. [CSP docs](https://v2.tauri.app/security/csp/)
- CSP applies to the main webview's documents; an `<iframe srcdoc>` document inherits the embedder's CSP (community-verified: "srcdoc iframes inherit the embedder's CSP" — [loci PR #83](https://github.com/huximaxi/loci/pull/83); same finding in [maru issue #410](https://github.com/STAIxBWLB/maru/issues/410)). Practical consequence: a per-message relaxed-but-restricted CSP cannot be expressed via srcdoc alone; use the custom-scheme route (section 5) if you need a different CSP per message.

## 3. Plugins and PTY

**Verdict: shell plugin has no PTY; use `portable-pty` (or the community `tauri-plugin-pty`) with a reader thread streaming to a Tauri `Channel`.**

- `tauri-plugin-shell` 2.4.0 (Rust, 2026-09-26) / `@tauri-apps/plugin-shell` 2.4.0. Its `Command` API gives pipes, not a pseudo-terminal — no PTY support. [crates.io](https://crates.io/crates/tauri-plugin-shell)
- `portable-pty` 0.9.0 (wezterm's crate, 2025-02-11) — stable but slow-moving (0.8.1 was 2023). [crates.io](https://crates.io/crates/portable-pty)
- `pty-process` 0.5.3 (2025-07-12) — lighter alternative, actively maintained. [crates.io](https://crates.io/crates/pty-process)
- `tauri-plugin-pty` (Tnze) — ready-made Tauri 2 plugin exposing `spawn("shell", args, {cols, rows})` to JS with `onData`/`write`, designed for xterm.js; on crates.io since early 2026. [GitHub](https://github.com/Tnze/tauri-plugin-pty), [lib.rs](https://lib.rs/crates/tauri-plugin-pty)
- Reference examples: `marc2332/tauri-terminal` (xterm.js + portable-pty, the canonical demo) and `thikander/volt-terminal` (Tauri 2 + portable-pty, "one reader thread per session forwarding output as events"). [tauri-terminal](https://github.com/marc2332/tauri-terminal), [volt-terminal](https://github.com/thikander/volt-terminal)
- Others: `@tauri-apps/plugin-clipboard-manager` 2.4.0, `plugin-notification` 2.5.0, `plugin-opener` 2.6.0, `plugin-window-state` 2.5.0. Notification plugin docs note the Rust MSRV 1.77.2 floor; on macOS delivering notifications to a not-notarised/ad-hoc-signed app is unreliable in practice — sign with your Developer ID for production ([plugin docs](https://v2.tauri.app/plugin/notification/)). `opener` is the current way to open links externally (replaces shell-open).

## 4. xterm.js and IPC throughput

**Verdict: `@xterm/xterm` 6.0.0 + webgl addon; use a Tauri `Channel`, not events; latency is fine for an embedded terminal.**

- `@xterm/xterm` 6.0.0 (published ~2025-12-22). Breaking: canvas renderer removed in 6.0.0 ([release notes, PR #5105](https://newreleases.io/project/github/xtermjs/xterm.js/release/6.0.0)); old `xterm`/`xterm-*` packages deprecated. Current addons on npm: `@xterm/addon-fit` 0.11.0, `@xterm/addon-webgl` 0.19.0, `@xterm/addon-canvas` 0.7.0 (legacy — pointless on 6.x). [npm](https://www.npmjs.com/package/@xterm/xterm)
- Renderer on WKWebView: webgl is the recommended/fast path (6.0 release work targeted webgl, e.g. shadow-DOM support PR #5334); DOM renderer is the built-in fallback. WebGL works in WKWebView on macOS 15.
- Measured numbers (Tauri-like architecture, Sep 2026): keystroke page→PTY→page round trip ≈ 1.0 ms; echo→WebGL draw 8.8 ms mean / 17 ms worst at 60 Hz; published xterm.js apps (Hyper) sit at 32–40 ms keystroke-to-glass vs 15 ms for a native terminal. Tauri IPC is not the bottleneck. [skiff issue #16](https://github.com/solutionscay/skiff/issues/16)
- Transport: Tauri docs explicitly recommend `Channel` over the event system "for ordered, high-throughput data delivery" — events can be processed out of order. [Calling Rust](https://v2.tauri.app/develop/calling-rust/), [IPC concept](https://v2.tauri.app/concept/inter-process-communication/). Pattern: `#[tauri::command] async fn pty_out(on_event: Channel<Vec<u8>>)` fed by the portable-pty reader thread.

## 5. WKWebView specifics (email HTML viewing)

**Verdict: sandboxed srcdoc works but inherits the app CSP; a custom async URI scheme gives you per-document headers — with one known macOS iframe pitfall.**

- `<iframe sandbox srcdoc>` renders in WKWebView; with `sandbox` (no `allow-same-origin`) the frame gets an opaque origin and no IPC reach into Tauri. CSP: the srcdoc document inherits the embedder's CSP, so the parent CSP is your only lever there (see section 2).
- `register_asynchronous_uri_scheme_protocol` / `register_uri_scheme_protocol` on `tauri::Builder` lets Rust serve arbitrary documents on a custom scheme, and the `http::Response` you return can carry its own `Content-Security-Policy` header — this is the clean way to serve per-message sanitised HTML with a message-specific CSP. [docs.rs Builder](https://docs.rs/tauri/latest/tauri/struct.Builder.html), [DeepWiki overview](https://deepwiki.com/tauri-apps/tauri/4.4-custom-protocol-handlers)
- Pitfall: loading HTML from a custom protocol into an `<iframe>` on macOS has had issues ([issue #12767](https://github.com/tauri-apps/tauri/issues/12767), Feb 2025). Spike should test `iframe src=custom://...` vs srcdoc-with-fetch early.
- Navigation interception: `WebviewWindowBuilder::on_navigation(|url| bool)` — returning false cancels; docs show the allowlist pattern (scheme `tauri`, or localhost in dev). [docs.rs WebviewWindowBuilder](https://docs.rs/tauri/latest/tauri/webview/struct.WebviewWindowBuilder.html)
- New-window (target=_blank) interception: no first-class `on_new_window` builder hook was verified; mark UNVERIFIED — mitigate by sanitising/removing target=_blank and routing links through `opener` yourself.

## 6. shadcn/ui, Tailwind, Radix

**Verdict: Tailwind 4 + current shadcn CLI is the default path, but shadcn moved its base to Base UI in mid-2026 — Radix is now opt-in.**

- Tailwind 4 is CSS-first: no required `tailwind.config.js`; theme lives in CSS via `@theme`; shadcn adds `@import "shadcn/tailwind.css"` to your global CSS on `init`. [shadcn Tailwind v4](https://ui.shadcn.com/docs/tailwind-v4), [CLI docs](https://ui.shadcn.com/docs/cli)
- shadcn changelog: Feb 2026 new-york style switched to the unified `radix-ui` package instead of individual `@radix-ui/react-*`; June 2026 "Base UI as the Default" — new projects default to Base UI components, Radix available as a variant/registry. Recent extras: `shadcn/registry`, `shadcn apply/preset`, `@shadcn/helpers`. [Changelog](https://ui.shadcn.com/docs/changelog), [Feb 2026 radix-ui entry](https://ui.shadcn.com/docs/changelog/2026-02-radix-ui)
- Consequence for the spike: `pnpm dlx shadcn@latest init` then `add` — expect `base-ui`/`radix-ui` unified imports, not dozens of `@radix-ui/react-*` packages.

## 7. macOS signing / notarisation

**Verdict: supported end-to-end by `tauri build` + `tauri-action`; budget CI time for notarisation and pin the exact env vars from the docs.**

- Official flow: Developer ID Application cert; `tauri.conf.json > bundle > macOS > signingIdentity`; notarisation via App Store Connect API key (`APPLE_API_ISSUER`, `APPLE_API_KEY`, `APPLE_API_KEY_PATH`) or Apple ID env vars. Notarisation is required with a Developer ID cert; free accounts cannot notarise. [macOS Code Signing](https://v2.tauri.app/distribute/sign/macos/) (updated May 2026)
- `tauri-action` on GitHub Actions picks up `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`/`APPLE_PASSWORD`/`APPLE_TEAM_ID` (or API key vars) and signs+notarises automatically. Community pitfalls: notarisation step can stall for tens of minutes ([discussion #8630](https://github.com/orgs/tauri-apps/discussions/8630)); App Store-style distribution needed extra `codesign --requirements` ([issue #15230](https://github.com/tauri-apps/tauri/issues/15230), Apr 2026); people report needing many secrets and a long workflow ([Reddit, Jan 2026](https://www.reddit.com/r/tauri/comments/1q8k2i5/building_notarizing_a_macos_app_is_way_harder/)).

## 8. Alternatives

**Verdict: nothing since 2024 argues against Tauri 2 for this project.**

- **egui / iced**: pure-Rust UI, smallest binaries, no webview. But you lose HTML/CSS rendering outright — sanitised email HTML would need re-rendering through a Rust layout engine (poor fidelity) or a hidden webview anyway; xterm embedding exists (iced_term/egui term widgets) but is far less mature than xterm.js. Wrong tool for an email client.
- **Electron (42.x in 2026)**: bundles Chromium — ~100 MB+ installs and high baseline memory, in exchange for uniform rendering. A 2026 comparison still shows Tauri apps ~96% smaller. Only makes sense if you wanted Chromium-only CSS/JS guarantees; you don't, and your backend is Rust. [comparison repo](https://github.com/Elanis/web-to-desktop-framework-comparison)
- **Wails (Go)**: same "system webview + thin backend" idea, but the backend is Go — it cannot link your Rust workspace crate by path, so your daemon-client logic gets reimplemented. Out.
- Since 2024 the main Tauri changes are the 2.x capability security model maturing and 3.0 entering alpha; 2.x remains fully maintained, and the PTY/terminal ecosystem (tauri-plugin-pty, worked examples) is now proven. Stay on 2.12.

## What to install for the spike

| Piece | Exact name / version |
|---|---|
| Scaffold | `pnpm create tauri-app@latest` (pnpm, TypeScript, React/Vite template) |
| Rust | `tauri = "2.12"`, `tauri-build = "2.12"` (MSRV 1.90 → use latest stable) |
| CLI | `@tauri-apps/cli@^2.12.0` + `tauri-cli` cargo equivalent if wanted |
| JS API | `@tauri-apps/api@^2.12.0` |
| Plugins (npm) | `@tauri-apps/plugin-shell@^2.4.0`, `@tauri-apps/plugin-clipboard-manager@^2.4.0`, `@tauri-apps/plugin-notification@^2.5.0`, `@tauri-apps/plugin-opener@^2.6.0`, `@tauri-apps/plugin-window-state@^2.5.0` |
| Plugins (Rust) | matching `tauri-plugin-shell = "2.4"`, `-clipboard-manager = "2.4"`, `-notification = "2.5"`, `-opener = "2.6"`, `-window-state = "2.5"` |
| PTY | `portable-pty = "0.9"` (or evaluate `tauri-plugin-pty` and `pty-process = "0.5"`); stream via `tauri::ipc::Channel` |
| Terminal | `@xterm/xterm@^6.0.0`, `@xterm/addon-fit@^0.11.0`, `@xterm/addon-webgl@^0.19.0` (skip addon-canvas on 6.x) |
| UI | `tailwindcss@^4`, `@tailwindcss/vite`, `shadcn` via `pnpm dlx shadcn@latest init` (Base UI default; `radix-ui` unified pkg if you choose the Radix style) |

## Sources
- Kept: crates.io API for tauri/tauri-cli/plugins/pty crates (exact versions + dates); npm registry `/latest` docs (exact npm versions); [capabilities](https://v2.tauri.app/security/capabilities/), [CSP](https://v2.tauri.app/security/csp/), [calling-rust](https://v2.tauri.app/develop/calling-rust/), [sign/macos](https://v2.tauri.app/distribute/sign/macos/), [create-project](https://v2.tauri.app/start/create-project/) (official docs); [tauri 3.0.0-alpha.3 release](https://github.com/tauri-apps/tauri/releases/tag/tauri-v3.0.0-alpha.3); [xterm.js 6.0.0 notes](https://newreleases.io/project/github/xtermjs/xterm.js/release/6.0.0); [skiff #16 latency measurements](https://github.com/solutionscay/skiff/issues/16); [tauri-plugin-pty](https://github.com/Tnze/tauri-plugin-pty); [tauri-terminal](https://github.com/marc2332/tauri-terminal), [volt-terminal](https://github.com/thikander/volt-terminal); [issue #12767](https://github.com/tauri-apps/tauri/issues/12767); [docs.rs WebviewWindowBuilder](https://docs.rs/tauri/latest/tauri/webview/struct.WebviewWindowBuilder.html); [shadcn changelog](https://ui.shadcn.com/docs/changelog) + [tailwind-v4](https://ui.shadcn.com/docs/tailwind-v4); [issue #15431 Catalina](https://github.com/tauri-apps/tauri/issues/15431).
- Dropped: SEO comparisons (tech-insider.org, digitalapplied.com, jb.desishub.com) — marketing-grade, kept only the Elanis measured-comparison repo; v1 Tauri docs (obsolete); Reddit/dev.to signing posts kept only as pitfall colour, not as authority.

## Gaps
- New-window (target=_blank / window.open) interception API on Tauri 2: UNVERIFIED — no first-class hook found in the pages fetched; next step: read `WebviewWindowBuilder` full docs / search `on_new_window` in the tauri repo.
- Whether the webgl addon is formally "recommended on WKWebView": inferred from xterm 6 release work and community reports (nexterm runs WebGL on WKWebView), not an official statement.
- Exact notification-plugin macOS signing requirement wording: plugin docs page fetched only partially; practical claim rests on community reports.

## M5 refresh, 2026-10-01

Re-checked against crates.io and npm for #0130 before the install, approved by Sylvain the same day; nothing moved since the research above.

- `portable-pty` 0.9.0 (2025-02-11): installed. `pty-process` 0.5.3 has no Windows path and `tauri-plugin-pty` 0.3.1 (2026-07-08) hides the reader thread the read batching needs, so both stay out.
- `@xterm/xterm` 6.0.0, `@xterm/addon-fit` 0.11.0, `@xterm/addon-webgl` 0.19.0, `@xterm/addon-unicode11` 0.9.0, `@xterm/addon-clipboard` 0.2.0 (npm entries last modified 2026-08-30): installed. unicode11 gives the width table Neovim assumes for CJK and emoji; clipboard answers the OSC 52 writes of Neovim's `+` register, the only clipboard path a PTY has.
- `libghostty-vt` stays out: the VT state machine alone, without a renderer, and the full libghostty is not published for embedding.
