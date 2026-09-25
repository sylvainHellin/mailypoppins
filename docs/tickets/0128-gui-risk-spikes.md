---
id: 0128
title: Phase 1b of the native GUI, the Tauri, PTY and Neovim risk spikes
type: idea
priority: next
status: open
created: 2026-09-25
---

First ticket of the GUI half of the [native GUI plan](../plans/native-gui.md), sections "Embedded Neovim / Feasibility spike" and "Phase 1b: GUI risk spikes".
It gates Phase 7 (#0129) and nothing else.
It runs on the Mac: the first GUI release is macOS-only, and Finder launch, signing and a real Neovim under a signed app cannot be exercised on the headless Linux server.

## Work

- Prototype a Tauri 2 shell, a native PTY, a terminal component in the webview and a real Neovim process, in a disposable directory outside the product tree (`spikes/gui-neovim/`, excluded from the workspace like `spikes/ipc-bench`).
- Validate every row of the plan's feasibility list: startup from a signed app, user config and plugins, Finder PATH resolution, PTY input, output, resize, clipboard, Unicode, IME, mouse and colour, keyboard routing between app and terminal, clean child termination on window close and on crash, a save reaching the daemon's draft watcher and coming back through the subscription, cold-start and typing latency.
- Dependency due diligence for the PTY crate, the terminal component, and the frontend stack (Tauri 2, React, Vite, shadcn/ui, Tailwind): current versions, maintenance, licence, platform evidence, recorded as a decision record under `docs/baselines/decisions/`, the format #0119 used.
- Nothing is installed before Sylvain approves the due-diligence record.

## Exit gate

- Embedded Neovim meets the input, resize, save and latency requirements, with the numbers committed as an artifact.
- The PTY and terminal dependencies have a recorded decision.
- No spike code is carried into the product tree; the spike directory is deleted or kept outside the workspace with a reason.

## Owner action carried here

The live launchd check from #0125 is NOT TAKEN and needs the Mac anyway: `mp daemon install-service`, log out and back in, `mp daemon status`, `mp daemon uninstall-service`.
Two questions ride on it: whether `bootstrap` refuses a label it has already loaded, and what `current_exe()` resolves to under a Homebrew `mp`.
