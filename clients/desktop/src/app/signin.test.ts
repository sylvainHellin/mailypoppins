import { describe, expect, it } from "vitest";
import { reducer, type Action } from "@/app/reducer";
import { deviceCodeOf, droppedLine, storedTokenLine } from "@/app/signin";
import { initialState, needsSetup, screenFor, type AppState } from "@/app/state";
import { fixtures, DEVICE_CODE_MESSAGE } from "@/test/tauri-mock";
import type { ConfigSnapshot } from "@/lib/gui-types";
import type { OperationStatus } from "@/protocol/types";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);

function booted(): AppState {
  return run(initialState(), { type: "gui_event", event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap } });
}

function envelope(kind: string, payload: unknown, revision = 900): Action {
  return { type: "gui_event", event: { type: "event", event: { instance_id: fixtures.bootstrap.instance_id, revision, kind, payload } } };
}

const progress = (id: string) => envelope("operation.progress", { operation_id: id, phase: "device_code", done: 0, total: null, message: DEVICE_CODE_MESSAGE }, 901);
const stored = { stored: true, account: "home", kind: "graph", key: "oauth2-token-home" };

describe("the sign-in model", () => {
  it("reads the code from its own operation's progress and settles stored", () => {
    let s = run(booted(), { type: "sign_in_requested", token: 1, account: "home" }, { type: "sign_in_started", token: 1, operation_id: "op-1" });
    expect(s.overlay).toBe("device_code");
    s = run(s, progress("op-other"));
    expect(deviceCodeOf(s)).toBeNull();
    s = run(s, progress("op-1"));
    expect(deviceCodeOf(s)).toEqual({ url: "https://microsoft.com/devicelogin", code: "FXTR-CODE" });
    s = run(s, envelope("operation.finished", { operation_id: "op-1", state: "succeeded", result: stored }, 902));
    expect(s.signIn?.outcome).toEqual({ kind: "stored", text: storedTokenLine("home") });
    expect(s.progress["op-1"]).toBeUndefined();
    s = run(s, { type: "sign_in_closed" });
    expect(s.signIn).toBeNull();
    expect(s.overlay).toBeNull();
  });

  it("a finish that overtook the start's answer settles it once the id is known", () => {
    let s = run(booted(), { type: "sign_in_requested", token: 1, account: "home" });
    s = run(s, envelope("operation.finished", { operation_id: "op-1", state: "succeeded", result: stored }));
    expect(s.signIn?.outcome).toBeNull();
    s = run(s, { type: "sign_in_started", token: 1, operation_id: "op-1" });
    expect(s.signIn?.outcome?.kind).toBe("stored");
  });

  it("settles through a re-bootstrap's operation_settled, and a restart drops it", () => {
    const status: OperationStatus = {
      operation_id: "op-1",
      method: "config.oauth2_login",
      state: "succeeded",
      scope: "durable",
      progress: null,
      result: stored,
      error: null,
    };
    const started = run(booted(), { type: "sign_in_requested", token: 1, account: "home" }, { type: "sign_in_started", token: 1, operation_id: "op-1" });
    const settled = run(started, { type: "gui_event", event: { type: "operation_settled", operation_id: "op-1", kind: "oauth2_login", status } });
    expect(settled.signIn?.outcome?.kind).toBe("stored");
    const dropped = run(started, {
      type: "gui_event",
      event: { type: "operation_dropped", operation_id: "op-1", kind: "oauth2_login", reason: "the daemon restarted" },
    });
    expect(dropped.signIn?.outcome).toEqual({ kind: "failed", text: droppedLine("home", "the daemon restarted") });
  });

  it("a refused start ends it with the daemon's sentence and a running one cannot be closed", () => {
    let s = run(booted(), { type: "sign_in_requested", token: 1, account: "work" }, { type: "sign_in_closed" });
    expect(s.signIn?.account).toBe("work");
    s = run(s, { type: "sign_in_failed", token: 1, error: { kind: "protocol", code: -32602, message: "Account 'work' uses auth_method = \"password\"" } });
    expect(s.signIn?.outcome?.kind).toBe("failed");
    expect(s.signIn?.outcome?.text).toContain("uses auth_method");
  });
});

describe("the first-run screen", () => {
  const absent: ConfigSnapshot = { revision: 0, path: "/c/config.toml", state: "absent", config: { secrets_backend: "", email: { send_hold_secs: 0 }, accounts: [] } };
  it("shows for a daemon with no account and no config.toml only", () => {
    const empty = { ...fixtures.bootstrap, snapshot: { ...fixtures.bootstrap.snapshot, accounts: [], mailboxes: {} } };
    let s = run(initialState(), { type: "gui_event", event: { type: "rebootstrapped", cause: "subscribed", bootstrap: empty } });
    expect(screenFor(s)).toBe("shell");
    s = run(s, { type: "config_loaded", gen: s.config.gen, snapshot: absent });
    expect(needsSetup(s)).toBe(true);
    expect(screenFor(s)).toBe("setup");
    s = run(s, { type: "config_loaded", gen: s.config.gen, snapshot: { ...absent, state: "ok" } });
    expect(screenFor(s)).toBe("shell");
    const withAccounts = run(booted(), { type: "config_loaded", gen: 1, snapshot: absent });
    expect(screenFor(withAccounts)).toBe("shell");
  });
});
