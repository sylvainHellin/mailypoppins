---
id: 0128
title: M0 of the native GUI, the Tauri, PTY and Neovim risk spikes
type: idea
priority: next
status: done
created: 2026-09-25
closed: 2026-09-30
---

Status: done. The spike ran on branch `gui-spike` (Tauri 2.12, portable-pty 0.9, xterm.js 6, React 19, macOS 26.6.2) and met every performance target it measured, in the release build and with the user's own Neovim configuration.
The reader route is decided: the custom `mpmsg` scheme with a per-message CSP header, a `sandbox="allow-popups"` frame, and `frame-src` open to http(s) so link clicks reach `on_navigation`; the plan's "Reading HTML bodies" and "Risks" sections carry it.
PTY output needs read batching before M5 (1.2 MB/s in release).
The Neovim rows the timebox did not reach move to M5, the dependency due-diligence record to M1, and the live launchd check to #0129.
The numbers and findings are in [docs/baselines/gui-spike-m0.md](../baselines/gui-spike-m0.md); the spike branch is kept as reference until M1 lands, and no spike code entered the product tree.

First ticket of the [native GUI plan](../plans/native-gui.md), milestone "M0: spike".
It gates M1 (#0129), and the later milestones follow M1 in order.
It runs on the Mac: the first GUI release is macOS-only, and Finder launch, signing and a real Neovim under a signed app cannot be exercised on the headless Linux server.

## Work

- Prototype a Tauri 2 shell, a native PTY, a terminal component in the webview and a real Neovim process, in a disposable directory outside the product tree (`spikes/gui-neovim/`, excluded from the workspace like `spikes/ipc-bench`).
- Validate every row of the plan's M0 list: a `state.bootstrap` connection listing accounts and mailboxes, `message.html` in a sandboxed iframe with link interception, and on the Neovim half startup from a signed app, user config and plugins, Finder PATH resolution, PTY input, output, resize, clipboard, Unicode, IME, mouse and colour, keyboard routing between app and terminal, clean child termination on window close and on crash, a save reaching the daemon's draft watcher and coming back through the subscription, cold-start and typing latency.
- Dependency due diligence for the PTY crate, the terminal component, and the frontend stack (Tauri 2, React, Vite, shadcn/ui, Tailwind): current versions, maintenance, licence, platform evidence, recorded as a decision record under `docs/baselines/decisions/`, the format #0119 used.
- Nothing is installed before Sylvain approves the due-diligence record.

## Exit gate

- Embedded Neovim meets the input, resize, save and latency requirements, with the numbers committed as an artifact.
- The PTY and terminal dependencies have a recorded decision.
- No spike code is carried into the product tree; the spike directory is deleted or kept outside the workspace with a reason.

## Owner action carried here

Not taken during M0; carried to #0129.
The live launchd check from #0125 is NOT TAKEN and needs the Mac anyway: `mp daemon install-service`, log out and back in, `mp daemon status`, `mp daemon uninstall-service`.
Two questions ride on it: whether `bootstrap` refuses a label it has already loaded, and what `current_exe()` resolves to under a Homebrew `mp`.
