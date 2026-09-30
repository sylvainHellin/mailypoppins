import { describe, expect, it } from "vitest";
import { reducer, type Action } from "@/app/reducer";
import { contactRows, cursorContact, EMPTY_INDEX, rebuildResult } from "@/app/contacts";
import { initialState, isStale, type AppState } from "@/app/state";
import { hiddenNotice, OPEN_CONTACTS_FIRST } from "@/app/views";
import { fixtures, formatRecipient } from "@/test/tauri-mock";
import type { ContactRebuilt, ContactSearch } from "@/lib/gui-types";
import type { OperationStatus } from "@/protocol/types";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);
const last = (s: AppState) => s.activity[s.activity.length - 1];

function booted(): AppState {
  return run(initialState(), {
    type: "gui_event",
    event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap },
  });
}

function envelope(kind: string, payload: unknown, revision = 900): Action {
  return { type: "gui_event", event: { type: "event", event: { instance_id: fixtures.bootstrap.instance_id, revision, kind, payload } } };
}

function search(account: string, query = ""): ContactSearch {
  const contacts = fixtures.contacts[account].map((c) => ({ ...c, recipient: formatRecipient(c.display_name, c.address) }));
  return { account, query, contacts };
}

/** The Contacts view open on `work`, its list read. */
function withContacts(): AppState {
  const s = run(booted(), { type: "switch_view", view: "contacts" });
  return run(s, { type: "contacts_loaded", account: "work", gen: s.contacts.work.gen, search: search("work") });
}

function rebuilt(patch: Partial<ContactRebuilt> = {}): ContactRebuilt {
  return { account: "work", contacts: 25, kept: 0, saved: "written", cache_path: "/c/work/contacts-cache.json", ...patch };
}

const finished = (operation_id: string, result: unknown, state = "succeeded", error: unknown = null) =>
  envelope("operation.finished", { operation_id, state, ...(result === null ? { error } : { result }) });

function status(operation_id: string, result: unknown): OperationStatus {
  return { operation_id, method: "contact.rebuild", state: "succeeded", scope: "durable", progress: null, result, error: null } as OperationStatus;
}

describe("the contact rebuild's notices, the TUI's words", () => {
  it("says how many a written index holds, and the cache guard's two refusals with what it kept", () => {
    expect(rebuildResult(rebuilt())).toEqual({ kind: "applied", text: "Contacts refreshed (25)" });
    expect(rebuildResult(rebuilt({ saved: "refused_empty", contacts: 0, kept: 25 }))).toEqual({
      kind: "rebuild_refused",
      text: "Contacts rebuild found none, kept 25 cached",
    });
    expect(rebuildResult(rebuilt({ saved: "refused_shrunk", contacts: 3, kept: 25 }))).toEqual({
      kind: "rebuild_refused",
      text: "Contacts rebuild found only 3, kept 25 cached",
    });
    // An unknown verdict reads as written, as the TUI's does.
    expect(rebuildResult(rebuilt({ saved: "later" })).text).toBe("Contacts refreshed (25)");
  });
});

describe("the Contacts view's model", () => {
  it("opens on the selection's account, reads its list on every open, and keeps the query", () => {
    let s = withContacts();
    expect(s.contactsView).toEqual({ account: "work", query: "", cursor: null, searching: false });
    expect(isStale(s.contacts.work)).toBe(false);
    s = run(s, { type: "contacts_query", query: "doe" });
    expect(s.contactsView?.query).toBe("doe");
    expect(isStale(s.contacts.work)).toBe(true);
    s = run(s, { type: "contacts_loaded", account: "work", gen: s.contacts.work.gen, search: search("work", "doe") });
    s = run(s, { type: "switch_view", view: "mail" }, { type: "switch_view", view: "contacts" });
    expect(isStale(s.contacts.work)).toBe(true);
    expect(s.contactsView?.query).toBe("doe");
  });

  it("drops the answer of a query typed over, so the list is asked again for the new one", () => {
    let s = run(booted(), { type: "switch_view", view: "contacts" });
    const asked = s.contacts.work.gen;
    s = run(s, { type: "contacts_query", query: "ro" });
    s = run(s, { type: "contacts_loaded", account: "work", gen: asked, search: search("work") });
    expect(isStale(s.contacts.work)).toBe(true);
    expect(s.contacts.work.data).toBeNull();
    s = run(s, { type: "contacts_loaded", account: "work", gen: s.contacts.work.gen, search: search("work", "ro") });
    const settled = s.contacts.work;
    s = run(s, { type: "contacts_loaded", account: "work", gen: asked, search: search("work") });
    s = run(s, { type: "contacts_failed", account: "work", gen: asked, error: { kind: "timeout", message: "late" } });
    expect(s.contacts.work).toBe(settled);
  });

  it("reads an account's list again when the view comes back to it with another query", () => {
    let s = run(withContacts(), { type: "select_account", account: "home" });
    s = run(s, { type: "contacts_loaded", account: "home", gen: s.contacts.home.gen, search: search("home") });
    s = run(s, { type: "contacts_query", query: "rob" });
    s = run(s, { type: "contacts_loaded", account: "home", gen: s.contacts.home.gen, search: search("home", "rob") });
    expect(isStale(s.contacts.work)).toBe(false);
    s = run(s, { type: "select_account", account: "work" });
    expect(s.contactsView).toMatchObject({ account: "work", query: "rob" });
    expect(isStale(s.contacts.work)).toBe(true);
  });

  it("moves its cursor with j, k, G and gg, by address, and the mail selection stays", () => {
    let s = withContacts();
    const message = s.selection.message;
    expect(cursorContact(s)?.address).toBe("robin@example.com");
    s = run(s, { type: "move_selection", to: 1, relative: true });
    expect(s.contactsView?.cursor).toBe("jane.doe@example.com");
    s = run(s, { type: "move_selection", to: "last", relative: false });
    expect(s.contactsView?.cursor).toBe("zoe@example.com");
    s = run(s, { type: "move_selection", to: "first", relative: false });
    expect(s.contactsView?.cursor).toBe("robin@example.com");
    expect(s.selection.message).toEqual(message);
    expect(contactRows(s)).toHaveLength(25);
  });

  it("follows the selection's account, and forgets a removed account's list", () => {
    let s = withContacts();
    s = run(s, { type: "select_account", account: "home" });
    expect(s.contactsView?.account).toBe("home");
    expect(s.contacts.home).toBeDefined();
    s = run(s, envelope("state.remove", { resource: "account:work" }));
    expect(s.contacts.work).toBeUndefined();
  });

  it("the contact actions say to open the view first from elsewhere", () => {
    const s = booted();
    expect(hiddenNotice(s, "contacts_copy")).toBe(OPEN_CONTACTS_FIRST);
    expect(hiddenNotice(withContacts(), "contacts_copy")).toBeNull();
  });

  it("has an empty-index sentence that names the rebuild key", () => {
    expect(EMPTY_INDEX).toBe("No contacts yet; press r to build the index");
  });
});

describe("the contact rebuilds this window awaits", () => {
  const requested = (token = 1): Action => ({ type: "rebuild_requested", token, account: "work" });

  it("a written rebuild says how many and reads the list again", () => {
    let s = run(withContacts(), requested(), { type: "rebuild_started", token: 1, operation_id: "op-r" });
    expect(s.rebuilds).toEqual([{ token: 1, account: "work", operation_id: "op-r" }]);
    s = run(s, finished("op-r", rebuilt()));
    expect(s.rebuilds).toEqual([]);
    expect(last(s)).toMatchObject({ kind: "applied", account: "work", text: "Contacts refreshed (25)" });
    expect(isStale(s.contacts.work)).toBe(true);
  });

  it("a refused rebuild warns and keeps the list as it is", () => {
    let s = run(withContacts(), requested(), { type: "rebuild_started", token: 1, operation_id: "op-r" });
    s = run(s, finished("op-r", rebuilt({ saved: "refused_shrunk", contacts: 3, kept: 25 })));
    expect(last(s)).toMatchObject({ kind: "rebuild_refused", text: "Contacts rebuild found only 3, kept 25 cached" });
    expect(isStale(s.contacts.work)).toBe(false);
  });

  it("a failed, a dropped and a refused start each say the refresh failed", () => {
    let s = run(withContacts(), requested(), { type: "rebuild_started", token: 1, operation_id: "op-r" });
    s = run(s, finished("op-r", null, "failed", { code: -32603, message: "the store is locked" }));
    expect(last(s)).toMatchObject({ kind: "failed", text: "Contacts refresh failed: the store is locked" });
    s = run(s, requested(2), { type: "rebuild_started", token: 2, operation_id: "op-d" });
    s = run(s, { type: "gui_event", event: { type: "operation_dropped", operation_id: "op-d", kind: "contact_rebuild", reason: "the daemon restarted" } });
    expect(last(s)).toMatchObject({ kind: "failed", text: "Contacts refresh failed: the daemon restarted" });
    s = run(s, requested(3), { type: "rebuild_failed", token: 3, error: { kind: "not_found", message: "account_unknown: work", code: -32005 } });
    expect(last(s)).toMatchObject({ kind: "failed", text: "Contacts refresh failed: account_unknown: work" });
    expect(s.rebuilds).toEqual([]);
  });

  it("an end that overtakes contact_rebuild's answer waits for its id", () => {
    let s = run(withContacts(), requested());
    s = run(s, finished("op-early", rebuilt({ contacts: 24 })));
    expect(s.rebuildEarly).toHaveLength(1);
    s = run(s, { type: "rebuild_started", token: 1, operation_id: "op-early" });
    expect(last(s)?.text).toBe("Contacts refreshed (24)");
    expect(s.rebuildEarly).toEqual([]);
    expect(s.rebuilds).toEqual([]);
  });

  it("the re-query's operation_settled settles it as contact_rebuild", () => {
    let s = run(withContacts(), requested(), { type: "rebuild_started", token: 1, operation_id: "op-q" });
    s = run(s, {
      type: "gui_event",
      event: { type: "operation_settled", operation_id: "op-q", kind: "contact_rebuild", status: status("op-q", rebuilt({ saved: "refused_empty", contacts: 0, kept: 25 })) },
    });
    expect(last(s)?.text).toBe("Contacts rebuild found none, kept 25 cached");
  });
});
