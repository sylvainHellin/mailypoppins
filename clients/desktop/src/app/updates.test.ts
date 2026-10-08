// The app update's model (ticket 0139): what the check answers, the event, the
// install's channel and the card's choices do to `state.update`.

import { describe, expect, it } from "vitest";
import { reducer, type Action } from "@/app/reducer";
import { initialState, type AppState } from "@/app/state";
import { updateEntryLabel, updatePaletteEntries, updateReason } from "@/app/updates";

/** The newest line of the activity log. */
const lastLine = (s: AppState) => s.activityLog[s.activityLog.length - 1];
const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);
const found: Action = { type: "update_available", update: { version: "0.12.0", notes: "Fixes", date: "2026-10-05T12:00:00Z" } };
const checked = (check: Partial<Extract<Action, { type: "update_checked" }>["check"]>, manual = true): Action => ({
  type: "update_checked",
  check: { state: "up_to_date", current: "0.11.0", ...check },
  manual,
});
const available = () => run(initialState(), found);
const downloading = () => run(available(), { type: "update_install_started", version: "0.12.0" });

describe("the update state", () => {
  it("starts idle, with nothing known about the running version", () => {
    const s = initialState();
    expect(s.update).toEqual({ kind: "idle" });
    expect(s.updateInfo).toBeNull();
  });

  it("update:available makes the update known", () => {
    expect(available().update).toEqual({ kind: "available", version: "0.12.0", notes: "Fixes", date: "2026-10-05T12:00:00Z" });
  });

  it("a manual check says how it went on the notice line and records the version and the last check", () => {
    const up = run(initialState(), checked({ last_check: "2026-10-05T12:00:00Z" }));
    expect(up.notice).toBe("mailypoppins 0.11.0 is up to date");
    expect(up.updateInfo).toEqual({ current: "0.11.0", last_check: "2026-10-05T12:00:00Z", reason: null });
    expect(up.update).toEqual({ kind: "idle" });

    const found = run(initialState(), checked({ state: "available", version: "0.12.0" }));
    expect(found.notice).toBe("Update 0.12.0 available");
    expect(found.update).toMatchObject({ kind: "available", version: "0.12.0" });

    const off = run(initialState(), checked({ state: "disabled", reason: "Updates are off in a development build." }));
    expect(off.notice).toBe("Updates are off in a development build.");
    expect(lastLine(off)).toMatchObject({ level: "warning", text: "Updates are off in a development build." });

    const failed = run(initialState(), checked({ state: "failed", reason: "error sending request" }));
    expect(failed.notice).toBe("The update check failed: error sending request");
    expect(lastLine(failed)).toMatchObject({ level: "error" });
  });

  it("a silent check says nothing, and its failure keeps the last check it had", () => {
    let s = run(initialState(), checked({ last_check: "2026-10-04T08:00:00Z" }, false));
    expect(s.notice).toBeNull();
    s = run(s, checked({ state: "failed", reason: "offline" }, false));
    expect(s.notice).toBeNull();
    expect(s.activityLog).toEqual([]);
    expect(s.updateInfo).toEqual({ current: "0.11.0", last_check: "2026-10-04T08:00:00Z", reason: null });
  });

  it("update_status records the line for Settings and seeds only a window that knows no update yet", () => {
    const off = run(initialState(), { type: "update_status", status: { current: "0.1.0", enabled: false, reason: "Updates are off in fixture mode." } });
    expect(off.updateInfo).toEqual({ current: "0.1.0", last_check: null, reason: "Updates are off in fixture mode." });
    expect(off.update).toEqual({ kind: "idle" });
    expect(off.notice).toBeNull();
    const held = { type: "update_status", status: { current: "0.11.0", enabled: true, installed: "0.12.0" } } as const;
    expect(run(downloading(), held).update.kind).toBe("downloading");
    expect(run(available(), held).update.kind).toBe("available");
  });

  it("a check that finds nothing forgets an available update, never a download or an installed one", () => {
    expect(run(available(), checked({})).update).toEqual({ kind: "idle" });
    expect(run(downloading(), checked({})).update.kind).toBe("downloading");
  });

  it("the install's channel moves the download, and finished asks for the restart", () => {
    let s = downloading();
    expect(s.update).toEqual({ kind: "downloading", version: "0.12.0", downloaded: 0 });
    s = run(s, { type: "update_progress", progress: { type: "started", content_length: 4_000_000 } });
    s = run(s, { type: "update_progress", progress: { type: "progress", downloaded: 1_000_000, content_length: 4_000_000 } });
    expect(s.update).toEqual({ kind: "downloading", version: "0.12.0", downloaded: 1_000_000, content_length: 4_000_000 });
    s = run(s, { type: "update_progress", progress: { type: "finished" } });
    expect(s.update).toEqual({ kind: "installed", version: "0.12.0", asking: true });
    expect(lastLine(s)?.text).toBe("mailypoppins 0.12.0 is installed; a restart runs it");
    // The command's answer after the channel's finished changes nothing.
    expect(run(s, { type: "update_installed" }).update).toEqual(s.update);
  });

  it("the install's answer ends the download when the channel's finished has not landed yet", () => {
    expect(run(downloading(), { type: "update_installed" }).update).toEqual({ kind: "installed", version: "0.12.0", asking: true });
  });

  it("Later keeps the update installed and stops asking; a new find leaves it alone", () => {
    const s = run(downloading(), { type: "update_installed" }, { type: "update_later" }, { type: "update_available", update: { version: "0.13.0" } });
    expect(s.update).toEqual({ kind: "installed", version: "0.12.0", asking: false });
  });

  it("a failed install keeps the reason, and Dismiss makes the update available again", () => {
    let s = run(downloading(), { type: "update_install_failed", reason: "signature mismatch" });
    expect(s.update).toEqual({ kind: "failed", version: "0.12.0", reason: "signature mismatch" });
    expect(lastLine(s)).toMatchObject({ level: "error", text: "The update to 0.12.0 failed: signature mismatch" });
    s = run(s, { type: "update_dismiss" });
    expect(s.update).toEqual({ kind: "available", version: "0.12.0" });
  });

  it("progress outside a download is dropped", () => {
    const s = available();
    expect(run(s, { type: "update_progress", progress: { type: "finished" } })).toBe(s);
    expect(run(s, { type: "update_install_failed", reason: "x" })).toBe(s);
  });

  it("restart with an editor open asks the leave question", () => {
    const s = run(initialState(), { type: "update_restart_asked" });
    expect(s.overlay).toBe("compose_leave");
    expect(s.composeLeave).toEqual({ kind: "restart" });
  });
});

describe("the update's entry points", () => {
  it("the sidebar entry names the version, offers the restart once installed, and hides otherwise", () => {
    expect(updateEntryLabel({ kind: "idle" })).toBeNull();
    expect(updateEntryLabel({ kind: "available", version: "0.12.0" })).toBe("Update to 0.12.0");
    expect(updateEntryLabel({ kind: "failed", version: "0.12.0", reason: "x" })).toBe("Update to 0.12.0");
    expect(updateEntryLabel({ kind: "downloading", version: "0.12.0", downloaded: 0 })).toBeNull();
    expect(updateEntryLabel({ kind: "installed", version: "0.12.0", asking: false })).toBe("Restart to finish the update");
  });

  it("the palette's update rows follow the state", () => {
    const labels = (u: AppState["update"]) => updatePaletteEntries(u).map((e) => [e.label, e.id]);
    expect(labels({ kind: "idle" })).toEqual([]);
    expect(labels({ kind: "available", version: "0.12.0" })).toEqual([["Update to v0.12.0", "update_install"]]);
    expect(labels({ kind: "downloading", version: "0.12.0", downloaded: 0 })).toEqual([]);
    expect(labels({ kind: "installed", version: "0.12.0", asking: true })).toEqual([["Restart to finish the update", "update_restart"]]);
  });

  it("a string rejection is the sentence itself", () => {
    expect(updateReason("mailypoppins 0.11.0 is up to date.")).toBe("mailypoppins 0.11.0 is up to date.");
    expect(updateReason({ kind: "internal", message: "bridge" })).toBe("bridge");
  });
});
