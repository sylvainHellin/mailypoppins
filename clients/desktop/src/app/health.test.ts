import { describe, expect, it } from "vitest";
import { reducer, type Action } from "@/app/reducer";
import { checkLabel, isHealthCheck, shownChecks, withCheck } from "@/app/health";
import { initialState, type AppState } from "@/app/state";
import type { Bootstrap, HealthCheck } from "@/protocol/types";
import { fixtures } from "@/test/tauri-mock";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);

const envelope = (kind: string, payload: unknown): Action => ({
  type: "gui_event",
  event: { type: "event", event: { instance_id: "fixture-instance-1", revision: 900, kind, payload } },
});

const boot = (diagnostics: HealthCheck[], instance_id = "fixture-instance-1"): Action => ({
  type: "gui_event",
  event: {
    type: "rebootstrapped",
    cause: "subscribed",
    bootstrap: { ...fixtures.bootstrap, instance_id, snapshot: { ...fixtures.bootstrap.snapshot, diagnostics } } as Bootstrap,
  },
});

const mailboxWarn: HealthCheck = {
  name: "config_loaded",
  status: "warn",
  detail: 'account "tum": unknown mailbox key `accounts.mailboxes.spesen` is ignored',
};
const storeFail: HealthCheck = { name: "store_open", status: "fail", detail: "0 of 1 account store(s) are open under /a" };
const lines = (s: AppState) => s.activityLog.map((e) => [e.level, e.text]);

describe("health checks", () => {
  it("seeds from the snapshot, logging each check once across a same-instance re-bootstrap", () => {
    const s = run(initialState(), boot([mailboxWarn]), boot([mailboxWarn]));
    expect(s.health).toEqual([mailboxWarn]);
    expect(lines(s)).toEqual([["warning", `config_loaded: ${mailboxWarn.detail}`]]);
  });

  it("a check_changed replaces, adds, and on ok drops by name, with a line each, the recovery included", () => {
    const edited = { ...mailboxWarn, detail: "account \"tum\": another key" };
    const s0 = run(initialState(), boot([mailboxWarn]));
    const s1 = run(s0, envelope("diagnostic.check_changed", storeFail), envelope("diagnostic.check_changed", edited));
    expect(s1.health).toEqual([edited, storeFail]);
    const s2 = run(s1, envelope("diagnostic.check_changed", { name: "config_loaded", status: "ok", detail: "/c/config.toml loaded, 2 account(s)" }));
    expect(s2.health).toEqual([storeFail]);
    expect(lines(s2).slice(1)).toEqual([
      ["error", `store_open: ${storeFail.detail}`],
      ["warning", `config_loaded: ${edited.detail}`],
      ["info", "config_loaded: /c/config.toml loaded, 2 account(s)"],
    ]);
  });

  it("a re-bootstrap's snapshot is the truth: a check that cleared while away goes", () => {
    const s = run(initialState(), boot([mailboxWarn, storeFail]), boot([storeFail], "fixture-instance-2"));
    expect(s.health).toEqual([storeFail]);
  });

  it("skips a payload that is not a check, as the TUI does", () => {
    const s0 = run(initialState(), boot([]));
    const s1 = run(s0, envelope("diagnostic.check_changed", { name: "x", status: "info", detail: "y" }), envelope("diagnostic.check_changed", null));
    expect(s1.health).toEqual([]);
    expect(s1.activityLog).toEqual([]);
    expect(isHealthCheck(mailboxWarn)).toBe(true);
  });

  it("leaves config_loaded to the config.toml banner while it shows", () => {
    const s = { ...initialState(), health: [mailboxWarn, storeFail] };
    expect(shownChecks(s)).toEqual([mailboxWarn, storeFail]);
    expect(shownChecks({ ...s, configProblem: { path: "/c", line: 3, message: "m", atStartup: false } })).toEqual([storeFail]);
  });

  it("a dismissed line stays hidden until its check's detail changes or it recovers", () => {
    const dismiss: Action = { type: "dismiss_check", name: storeFail.name, status: storeFail.status, detail: storeFail.detail };
    const s0 = run(initialState(), boot([mailboxWarn, storeFail]), dismiss);
    expect(shownChecks(s0)).toEqual([mailboxWarn]);
    // The same sentence again, by event or by a reconnect's snapshot, stays hidden.
    const s1 = run(s0, envelope("diagnostic.check_changed", storeFail), boot([mailboxWarn, storeFail]));
    expect(shownChecks(s1)).toEqual([mailboxWarn]);
    // A new detail shows.
    const moved = { ...storeFail, detail: "0 of 2 account store(s) are open under /a" };
    expect(shownChecks(run(s1, envelope("diagnostic.check_changed", moved)))).toEqual([mailboxWarn, moved]);
    // A recovery lets go, so the old sentence coming back shows again.
    const back = run(s1, envelope("diagnostic.check_changed", { ...storeFail, status: "ok" }), envelope("diagnostic.check_changed", storeFail));
    expect(back.healthDismissed).toEqual({});
    expect(shownChecks(back)).toEqual([mailboxWarn, storeFail]);
    // A click on a line whose detail already moved hides nothing.
    expect(run(s0, { type: "dismiss_check", name: mailboxWarn.name, status: "warn", detail: "stale" }).healthDismissed).toEqual({
      [storeFail.name]: { status: storeFail.status, detail: storeFail.detail },
    });
  });

  it("a dismissed warning shows again when the same sentence turns into a failure", () => {
    const warn: HealthCheck = { name: "account:home", status: "warn", detail: "blocked: the store is locked" };
    const fail: HealthCheck = { ...warn, status: "fail" };
    const s0 = run(initialState(), boot([warn]), { type: "dismiss_check", name: warn.name, status: warn.status, detail: warn.detail });
    expect(shownChecks(s0)).toEqual([]);
    const s1 = run(s0, envelope("diagnostic.check_changed", fail));
    expect(shownChecks(s1)).toEqual([fail]);
    expect(s1.healthDismissed).toEqual({});
    // A click on a line whose status already moved hides nothing.
    expect(run(s1, { type: "dismiss_check", name: warn.name, status: "warn", detail: warn.detail }).healthDismissed).toEqual({});
  });

  it("orders and labels: a new check joins the end, a known one keeps its place", () => {
    expect(withCheck([mailboxWarn, storeFail], { ...mailboxWarn, status: "fail" }).map((c) => c.status)).toEqual(["fail", "fail"]);
    expect(checkLabel("config_loaded")).toBe("config.toml");
    expect(checkLabel("account:home")).toBe("Account home");
    expect(checkLabel("something_new")).toBe("something_new");
  });
});
