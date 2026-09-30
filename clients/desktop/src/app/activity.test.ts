import { describe, expect, it } from "vitest";
import { reducer, type Action } from "@/app/reducer";
import {
  ACTIVITY_LOG_CAP,
  configChangedLine,
  configInvalidLine,
  FAILURES,
  filterLog,
  logActivity,
  noticeLevel,
  syncLine,
} from "@/app/activity";
import { senderAddress } from "@/app/interop";
import { loadPrefs, PREFS_KEY } from "@/app/prefs";
import { DEFAULT_PREFS, initialState, visibleNotices, type AppState } from "@/app/state";
import type { SyncCompleted } from "@/protocol/types";
import { fixtures } from "@/test/tauri-mock";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);

const envelope = (kind: string, payload: unknown): Action => ({
  type: "gui_event",
  event: { type: "event", event: { instance_id: "fixture-instance-1", revision: 900, kind, payload } },
});

const tick = (over: Partial<SyncCompleted> = {}): SyncCompleted => ({
  account: "work",
  severity: "ok",
  saved: 2,
  skipped: 40,
  flags_updated: 0,
  pruned: 0,
  prunes_deferred: 0,
  uid_rebound: 0,
  uidvalidity_resets: 0,
  bodies_truncated: 0,
  non_converging: [],
  failed_mutations: 0,
  error: null,
  new_inbox_mail: [],
  ...over,
});

const hold = fixtures.bootstrap.snapshot.holds[0];
const lines = (s: AppState) => s.activityLog.map((e) => [e.level, e.text]);

describe("the activity log", () => {
  it("keeps the newest ACTIVITY_LOG_CAP lines, oldest first, with rising ids", () => {
    expect(ACTIVITY_LOG_CAP).toBe(100);
    let s = initialState();
    for (let i = 1; i <= ACTIVITY_LOG_CAP + 5; i++) s = logActivity(s, "info", `line ${i}`);
    expect(s.activityLog).toHaveLength(ACTIVITY_LOG_CAP);
    expect(s.activityLog[0].text).toBe("line 6");
    expect(s.activityLog[ACTIVITY_LOG_CAP - 1].text).toBe(`line ${ACTIVITY_LOG_CAP + 5}`);
    const ids = s.activityLog.map((e) => e.id);
    expect(ids).toEqual([...ids].sort((a, b) => a - b));
    expect(s.activityLogSeq).toBe(ACTIVITY_LOG_CAP + 5);
    expect(new Date(s.activityLog[0].at).toISOString()).toBe(s.activityLog[0].at);
  });

  it("logs every notice line as information, or at the level it names, and never the clearing", () => {
    const s = run(
      initialState(),
      { type: "notice", text: "Copied mp://work/inbox/1" },
      { type: "notice", text: null },
      { type: "notice", text: "No log file found at /x", level: "warning" },
      { type: "notice", text: "Open config failed: no editor", level: "error" },
    );
    expect(lines(s)).toEqual([
      ["info", "Copied mp://work/inbox/1"],
      ["warning", "No log file found at /x"],
      ["error", "Open config failed: no editor"],
    ]);
    expect(s.notice).toBe("Open config failed: no editor");
  });

  it("logs a failure notice of the activity area as an error, with its rows, and keeps it once dismissed", () => {
    for (const kind of FAILURES) expect(noticeLevel(kind)).toBe("error");
    expect(noticeLevel("applied")).toBe("info");
    let s = run(
      initialState(),
      { type: "activity", kind: "applied", account: "work", text: "Archived 1 message" },
      {
        type: "activity",
        kind: "failed",
        account: "work",
        text: "Could not archive 1 message",
        rows: [{ key: "work#1", label: "Hello", reason: "no such mailbox" }],
      },
    );
    expect(lines(s)).toEqual([
      ["info", "Archived 1 message"],
      ["error", "Could not archive 1 message (Hello: no such mailbox)"],
    ]);
    s = run(s, { type: "dismiss_all_notices" });
    expect(s.activity).toEqual([]);
    expect(s.activityLog).toHaveLength(2);
  });

  it("logs sync.completed at the daemon's severity, config.changed and config.invalid", () => {
    const s = run(
      initialState(),
      envelope("sync.completed", tick()),
      envelope("sync.completed", tick({ account: "home", severity: "warning", failed_mutations: 1 })),
      envelope("sync.completed", tick({ severity: "error", error: "login refused" })),
      envelope("config.changed", { added: ["new"], updated: [], removed: ["old"], config_revision: 2 }),
      envelope("config.invalid", { path: "/c/config.toml", line: 7, message: "expected a table" }),
      envelope("draft.changed", { account: "work", id: "x", path: "/d/x.md" }),
    );
    expect(lines(s)).toEqual([
      ["info", "Synced work: 2 new, 40 existing"],
      ["warning", "Synced home: 2 new, 40 existing; 1 mutation(s) failed and were rolled back (see the log)"],
      ["error", "Fetch failed (work): login refused"],
      ["info", "Configuration reloaded: added new; removed old"],
      ["error", "The configuration was refused (/c/config.toml, line 7): expected a table"],
    ]);
    expect(configChangedLine({ added: [], updated: [], removed: [], config_revision: 1 })).toBe("Configuration reloaded: no account changed");
    expect(configInvalidLine({ path: "/c", line: null, message: "m" })).toBe("The configuration was refused (/c): m");
    expect(syncLine(tick({ non_converging: ["INBOX"], pruned: 3 }))).toBe(
      "Synced work: 2 new, 40 existing, 3 no longer in this mailbox, fetch not converging on INBOX (see the log)",
    );
  });

  it("logs a hold's end once: fired by its event, cancelled by its notice", () => {
    const booted = run(initialState(), {
      type: "gui_event",
      event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap },
    });
    const fired = run(booted, envelope("send.hold_fired", { ...hold, remaining_secs: 0 }), envelope("send.hold_fired", { ...hold, remaining_secs: 0 }));
    expect(lines(fired)).toEqual([["info", `Hold over, sending "${hold.subject}" from work`]]);
    const cancelled = run(
      booted,
      envelope("send.hold_cancelled", { ...hold, remaining_secs: 0 }),
      { type: "hold_cancel_answered", operation_id: hold.operation_id, cancelled: true },
    );
    expect(lines(cancelled)).toEqual([["info", `Send of "${hold.subject}" cancelled`]]);
  });

  it("filters on the text or the level's name, case-insensitively", () => {
    const s = run(
      initialState(),
      { type: "notice", text: "Copied the subject" },
      { type: "notice", text: "Open log failed: gone", level: "error" },
      { type: "notice", text: "No log file found", level: "warning" },
    );
    expect(filterLog(s.activityLog, "").map((e) => e.text)).toHaveLength(3);
    expect(filterLog(s.activityLog, "  LOG ").map((e) => e.text)).toEqual(["Open log failed: gone", "No log file found"]);
    expect(filterLog(s.activityLog, "error").map((e) => e.text)).toEqual(["Open log failed: gone"]);
    expect(filterLog(s.activityLog, "warn").map((e) => e.text)).toEqual(["No log file found"]);
    expect(filterLog(s.activityLog, "nothing")).toEqual([]);
  });
});

describe("hiding the activity area's notices", () => {
  it("toggles prefs.activityHidden, which hides the notices and keeps them in the model", () => {
    let s = run(initialState(), { type: "activity", kind: "failed", account: "work", text: "Could not archive 1 message" });
    expect(visibleNotices(s)).toHaveLength(1);
    s = run(s, { type: "toggle_activity_hidden" });
    expect(s.prefs.activityHidden).toBe(true);
    expect(visibleNotices(s)).toEqual([]);
    expect(s.activity).toHaveLength(1);
    s = run(s, { type: "toggle_activity_hidden" });
    expect(visibleNotices(s)).toHaveLength(1);
  });

  it("reads the stored preference, and defaults to shown", () => {
    const store = new Map<string, string>();
    const storage = { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => void store.set(k, v) } as Storage;
    expect(loadPrefs(storage)).toEqual(DEFAULT_PREFS);
    store.set(PREFS_KEY, JSON.stringify({ activityHidden: true }));
    expect(loadPrefs(storage).activityHidden).toBe(true);
    store.set(PREFS_KEY, JSON.stringify({ activityHidden: "yes" }));
    expect(loadPrefs(storage).activityHidden).toBe(false);
  });
});

describe("the sender's address", () => {
  it("is what the angle brackets hold, else the whole field", () => {
    expect(senderAddress('"Meyer, Robin" <robin@example.com>')).toBe("robin@example.com");
    expect(senderAddress("Robin <robin@example.com> ")).toBe("robin@example.com");
    expect(senderAddress("  robin@example.com ")).toBe("robin@example.com");
    expect(senderAddress("Robin")).toBe("Robin");
  });
});
