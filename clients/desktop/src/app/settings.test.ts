import { describe, expect, it } from "vitest";
import { reducer, type Action } from "@/app/reducer";
import { initialState, isStale, listKey, type AppState } from "@/app/state";
import { storedLine, swapLine, usesPassword } from "@/app/settings";
import { fixtures, mailboxListing } from "@/test/tauri-mock";
import type { AccountInfo, MessageList } from "@/lib/gui-types";
import type { MessageListRow } from "@/protocol/types";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);

const envelope = (kind: string, payload: unknown, revision = 700): Action => ({
  type: "gui_event",
  event: { type: "event", event: { instance_id: "fixture-instance-1", revision, kind, payload } },
});

const accounts: AccountInfo[] = fixtures.accounts.map((a) => ({
  name: a.name,
  default: a.default,
  backend: a.backend,
  runtime_state: a.state,
  sync_health: "ok",
  outbox: { open: 0, failed: 0, partial: 0 },
})) as unknown as AccountInfo[];

function booted(): AppState {
  let s = run(initialState(), {
    type: "gui_event",
    event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap },
  });
  s = run(s, { type: "accounts_loaded", gen: s.accounts.gen, accounts });
  for (const name of ["work", "home"]) s = run(s, { type: "mailboxes_loaded", account: name, gen: 1, listing: mailboxListing(name) });
  const rows = fixtures.messages.work.inbox as MessageListRow[];
  const list: MessageList = { kind: "messages", account: "work", mailbox: "inbox", total: rows.length, rows };
  return run(s, { type: "messages_loaded", key: listKey("work", "inbox"), gen: s.messages.gen, list });
}

const invalid = { path: "/c/config.toml", line: 30, message: "key with no value, expected `=`" };

describe("the configuration in the model", () => {
  it("config.invalid sets the banner's problem and config.changed clears it and reads the configuration again", () => {
    let s = run(booted(), { type: "switch_view", view: "settings" });
    s = run(s, { type: "config_loaded", gen: s.config.gen, snapshot: { revision: 0, path: "/c/config.toml", state: "ok", config: fixtures.config } });
    expect(isStale(s.config)).toBe(false);
    s = run(s, envelope("config.invalid", invalid));
    expect(s.configProblem).toEqual(invalid);
    // A refused file changes nothing the daemon serves.
    expect(isStale(s.config)).toBe(false);
    s = run(s, envelope("config.changed", { added: [], updated: [], removed: [], config_revision: 1 }));
    expect(s.configProblem).toBeNull();
    expect(isStale(s.config)).toBe(true);
    expect(isStale(s.accounts)).toBe(true);
  });

  it("config.changed reads the updated account's mailboxes and list again, and leaves the others", () => {
    let s = booted();
    expect(isStale(s.mailboxes.work)).toBe(false);
    s = run(s, envelope("config.changed", { added: ["new"], updated: ["work"], removed: [], config_revision: 1 }));
    expect(isStale(s.mailboxes.work)).toBe(true);
    expect(isStale(s.mailboxes.home)).toBe(false);
    expect(isStale(s.messages)).toBe(true);
    expect(s.mailboxes.new).toBeUndefined();
  });

  it("an account the daemon removed goes once: by its state.remove, and config.changed finds it gone", () => {
    let s = run(booted(), { type: "select_account", account: "home" });
    s = run(s, envelope("state.remove", { resource: "account:home" }, 701));
    expect(s.accounts.data?.map((a) => a.name)).toEqual(["work"]);
    expect(s.selection.account).toBe("work");
    const before = s;
    s = run(s, envelope("config.changed", { added: [], updated: [], removed: ["home"], config_revision: 1 }, 702));
    expect(s.accounts.data).toBe(before.accounts.data);
    expect(s.mailboxes).toBe(before.mailboxes);
    expect(s.selection).toBe(before.selection);
  });

  it("config.changed removes an account whose state.remove this window never saw", () => {
    let s = run(booted(), { type: "select_account", account: "home" });
    s = run(s, envelope("config.changed", { added: [], updated: [], removed: ["home"], config_revision: 1 }));
    expect(s.accounts.data?.map((a) => a.name)).toEqual(["work"]);
    expect(s.mailboxes.home).toBeUndefined();
    expect(s.selection.account).toBe("work");
  });

  it("the Settings view reads the configuration on every open", () => {
    let s = run(booted(), { type: "switch_view", view: "settings" });
    s = run(s, { type: "config_loaded", gen: s.config.gen, snapshot: { revision: 0, path: "/c", state: "ok", config: fixtures.config } });
    s = run(s, { type: "switch_view", view: "mail" }, { type: "switch_view", view: "settings" });
    expect(isStale(s.config)).toBe(true);
    expect(s.focus).toBe("list");
  });

  it("the password dialog holds its target only, and closing the overlay forgets it", () => {
    let s = run(booted(), { type: "open_password", account: "work", kind: "imap" });
    expect(s.overlay).toBe("password");
    expect(s.passwordDialog).toEqual({ account: "work", kind: "imap" });
    s = run(s, { type: "overlay", overlay: null });
    expect(s.passwordDialog).toBeNull();
  });

  it("a reload's notice is the log's words and adds no second log line", () => {
    let s = run(booted(), envelope("config.changed", { added: ["x"], updated: [], removed: ["y"], config_revision: 1 }));
    const lines = s.activityLog.length;
    s = run(s, { type: "config_reloaded", text: swapLine({ added: ["x"], updated: [], removed: ["y"] }) });
    expect(s.notice).toBe("Configuration reloaded: added x; removed y");
    expect(s.activityLog).toHaveLength(lines);
    expect(s.activityLog[lines - 1].text).toBe(s.notice);
  });

  it("names the stored password without its value, and knows which accounts have passwords", () => {
    expect(storedLine("work", "smtp")).toBe("Stored the SMTP password for work");
    expect(fixtures.config.accounts.map(usesPassword)).toEqual([true, false]);
  });
});
