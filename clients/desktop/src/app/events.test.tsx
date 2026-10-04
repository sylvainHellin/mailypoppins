import { act, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { setWidth } from "@/test/setup";
import { emit, emitEnvelope, fixtures, mock, resetMock } from "@/test/tauri-mock";
import { AppShell } from "@/components/shell/AppShell";
import { TooltipProvider } from "@/components/ui/tooltip";
import { StoreProvider, useAppState, useDispatch } from "@/app/store";
import type { Action } from "@/app/reducer";
import { useMutations, type Mutations } from "@/app/mutations";
import type { AppState } from "@/app/state";

const listCalls = () => mock.calls.filter((c) => c.cmd === "list_messages").length;

describe("live events end to end", () => {
  it("refetches the shown list when its mailbox is invalidated", async () => {
    renderApp();
    await shellReady();
    const before = listCalls();
    act(() =>
      emit({
        type: "event",
        event: { instance_id: "fixture-instance-1", revision: 101, kind: "state.invalidate", payload: { resource: "mailbox:work/inbox", scope: { query: "counts" } } },
      }),
    );
    await waitFor(() => expect(listCalls()).toBeGreaterThan(before));
  });

  it("keeps the open message across a daemon restart that renumbered the rows", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jj");
    const reader = screen.getByRole("complementary", { name: "Reader" });
    expect(await within(reader).findByRole("heading", { name: "Angebot Dachsanierung" })).toBeInTheDocument();

    mock.rowShift = 500;
    act(() => emit({ type: "disconnected", reason: "fixture: simulated daemon restart" }));
    act(() => emit({ type: "reconnected", instance_id: "fixture-instance-2" }));
    act(() =>
      emit({ type: "rebootstrapped", cause: "instance_changed", bootstrap: { ...fixtures.bootstrap, instance_id: "fixture-instance-2" } }),
    );
    await waitFor(() =>
      expect(document.querySelector('[role="option"][aria-selected="true"]')?.getAttribute("data-row-id")).toBe("1502"),
    );
    expect(await within(reader).findByRole("heading", { name: "Angebot Dachsanierung" })).toBeInTheDocument();
    expect(mock.calls.some((c) => c.cmd === "message_html_meta" && c.args?.row_id === 1502)).toBe(true);
    expect(within(reader).getByTitle(/^Message body/)).toHaveAttribute("src", "mpmsg://localhost/work/1502");
    expect(screen.queryByText(/Resynchronising|Reconnecting/)).toBeNull();
  });
});

describe("draft events and the editor", () => {
  const banner = () => document.querySelector('[data-slot="editing-banner"]') as HTMLElement;

  it("a draft.changed from the editor re-reads the Drafts list and keeps the editing banner", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("2");
    await screen.findByRole("listbox", { name: "Drafts messages" });
    await user.keyboard("j");
    await user.keyboard("e");
    await waitFor(() => expect(banner()).toHaveTextContent("Editing angebot-antwort.md in code --wait"));
    const before = listCalls();
    // What the watcher publishes for a save: the row as it now reads.
    mock.drafts.work.drafts[0].subject = "Re: Angebot Dachsanierung (v2)";
    act(() =>
      emitEnvelope("draft.changed", {
        account: "work",
        id: "angebot-antwort",
        path: "/fixture/work/drafts/angebot-antwort.md",
        to: "robin@example.com",
        subject: "Re: Angebot Dachsanierung (v2)",
        status: "draft",
        valid: true,
        ready: true,
      }),
    );
    await waitFor(() => expect(listCalls()).toBeGreaterThan(before));
    expect(await screen.findByRole("option", { name: /Angebot Dachsanierung \(v2\)/ })).toBeInTheDocument();
    expect(banner()).toHaveTextContent("Editing angebot-antwort.md");
  });

  it("a draft.invalid from the editor turns the row invalid and keeps it selected", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("2");
    await screen.findByRole("listbox", { name: "Drafts messages" });
    await user.keyboard("j");
    await user.keyboard("e");
    await waitFor(() => expect(banner()).toHaveTextContent("Editing angebot-antwort.md"));
    const listing = mock.drafts.work;
    listing.drafts = listing.drafts.filter((d) => d.id !== "angebot-antwort");
    listing.skipped = [{ path: "/fixture/work/drafts/angebot-antwort.md", error: "line 1: did not find expected key" }];
    act(() =>
      emitEnvelope("draft.invalid", {
        account: "work",
        id: "angebot-antwort",
        path: "/fixture/work/drafts/angebot-antwort.md",
        diagnostics: [{ line: 1, message: "did not find expected key" }],
      }),
    );
    const row = await screen.findByRole("option", { name: /invalid: line 1: did not find expected key/ });
    expect(row).toHaveAttribute("aria-selected", "true");
    expect(banner()).toHaveTextContent("Editing angebot-antwort.md");
  });
});

/** The app with a probe that reads the model and holds the bound mutations. */
function renderProbed() {
  resetMock();
  setWidth(1400);
  const probe = {} as { state: AppState; m: Mutations; dispatch: (a: Action) => void };
  function Probe() {
    probe.state = useAppState();
    probe.m = useMutations();
    probe.dispatch = useDispatch();
    return null;
  }
  render(
    <StoreProvider>
      <TooltipProvider>
        <AppShell />
        <Probe />
      </TooltipProvider>
    </StoreProvider>,
  );
  return probe;
}

const shownRows = () => [...document.querySelectorAll('[role="option"][data-row-id]')].map((e) => Number(e.getAttribute("data-row-id")));

describe("mutations end to end", () => {
  it("archives a row, and the drain's invalidation keeps it gone", async () => {
    const probe = renderProbed();
    await shellReady();
    await act(() => probe.m.archive([{ account: "work", row_id: 1002 }]));
    expect(shownRows()).not.toContain(1002);
    const before = listCalls();
    act(() => emitEnvelope("state.invalidate", { resource: "mailbox:work/inbox", scope: { query: "counts" } }));
    await waitFor(() => expect(listCalls()).toBeGreaterThan(before));
    await waitFor(() => expect(probe.state.messages.loadedGen).toBe(probe.state.messages.gen));
    expect(shownRows()).not.toContain(1002);
    expect(probe.state.activity.map((n) => n.text)).toEqual(["Archived 1 message"]);
  });

  it("drops a list read that started before the archive and landed after it", async () => {
    const probe = renderProbed();
    await shellReady();
    let open!: () => void;
    mock.gates.set("list_messages", new Promise<void>((r) => (open = r)));
    const before = listCalls();
    act(() => emitEnvelope("state.invalidate", { resource: "mailbox:work/inbox", scope: {} }));
    await waitFor(() => expect(listCalls()).toBe(before + 1));

    await act(() => probe.m.archive([{ account: "work", row_id: 1002 }]));
    expect(shownRows()).not.toContain(1002);
    await act(async () => open());
    expect(shownRows()).not.toContain(1002);
    // The dropped answer asked for a fresh read, which the archive is in.
    await waitFor(() => expect(listCalls()).toBe(before + 2));
    await waitFor(() => expect(probe.state.messages.loadedGen).toBe(probe.state.messages.gen));
    expect(shownRows()).not.toContain(1002);
  });

  it("brings rows back on a rollback and says so", async () => {
    const probe = renderProbed();
    await shellReady();
    await act(() => probe.m.archive([{ account: "work", row_id: 1002 }]));
    // The fixture's rollback: the row is back where it was, then the event.
    const archived = mock.rows.work.archive.findIndex((r) => r.id === 1002);
    const [back] = mock.rows.work.archive.splice(archived, 1);
    mock.rows.work.inbox.splice(1, 0, back);
    act(() => emitEnvelope("mutations.rolled_back", { account: "work", failed: 1 }));
    await waitFor(() => expect(shownRows()).toContain(1002));
    expect(probe.state.activity.map((n) => n.kind)).toEqual(["applied", "rolled_back"]);
  });

  it("counts a hold down from its events and cancels it", async () => {
    const probe = renderProbed();
    await shellReady();
    expect(probe.state.holds["fixture-hold-seed"]?.state).toBe("started");
    const hold = { ...fixtures.bootstrap.snapshot.holds[0], operation_id: "op-armed", hold_secs: 10, remaining_secs: 10 };
    mock.holds.push(hold);
    act(() => emitEnvelope("send.hold_started", hold));
    act(() => emitEnvelope("send.hold_tick", { ...hold, remaining_secs: 9 }));
    expect(probe.state.holds["op-armed"]).toMatchObject({ state: "tick", remaining_secs: 9 });
    await act(() => probe.m.cancelHold("op-armed"));
    expect(probe.state.holds["op-armed"]).toMatchObject({ state: "cancelled", remaining_secs: 0 });
    act(() => emitEnvelope("send.hold_fired", { ...fixtures.bootstrap.snapshot.holds[0], remaining_secs: 0 }));
    expect(probe.state.holds["fixture-hold-seed"].state).toBe("fired");
    expect(probe.state.activity.filter((n) => n.kind === "hold_cancelled")).toHaveLength(1);
  });

  it("discards a draft, and the state.remove that follows reloads the list without it", async () => {
    const probe = renderProbed();
    await shellReady();
    const drafts = () => {
      const list = probe.state.messages.data;
      return list?.kind === "drafts" ? list.listing.drafts.map((d) => d.id) : null;
    };
    act(() => probe.dispatch({ type: "select_mailbox", account: "work", slug: "drafts" }));
    await waitFor(() => expect(drafts()).toEqual(["angebot-antwort", "offsite-note"]));
    const before = listCalls();
    await act(() => probe.m.discardDrafts("work", ["offsite-note"]));
    expect(drafts()).toEqual(["angebot-antwort"]);
    await waitFor(() => expect(listCalls()).toBeGreaterThan(before));
    await waitFor(() => expect(probe.state.messages.loadedGen).toBe(probe.state.messages.gen));
    expect(drafts()).toEqual(["angebot-antwort"]);
    expect(probe.state.activity.map((n) => n.text)).toEqual(["Discarded 1 draft"]);
  });
});

// PERSO-80: the daemon's watcher ingests new mail and publishes a
// `sync.completed` that names the account only, with the wire's full payload.
describe("new mail from a watcher tick", () => {
  const tick = (account: string, subjects: string[] = []) => ({
    account,
    severity: "ok",
    saved: subjects.length,
    skipped: 0,
    flags_updated: 0,
    pruned: 0,
    prunes_deferred: 0,
    uid_rebound: 0,
    uidvalidity_resets: 0,
    bodies_truncated: 0,
    non_converging: [],
    failed_mutations: 0,
    error: null,
    new_inbox_mail: subjects.map((subject) => ({ from: "Nina <nina@example.com>", subject })),
  });
  /** The watcher's ingest: a new row at the top of a mailbox. */
  const arrive = (account: string, mailbox: string, id: number, subject: string) => {
    const rows = mock.rows[account][mailbox];
    const tmpl = rows[0];
    rows.unshift({ ...tmpl, id, uid: id, subject, message_id: `<new-${id}@fixture.example>`, selector: `mp://${account}/${mailbox}/new-${id}`, flags: { ...tmpl.flags, seen: false } });
  };

  it("shows a row the tick ingested into the open mailbox", async () => {
    const probe = renderProbed();
    await shellReady();
    expect(shownRows()).not.toContain(2001);
    arrive("work", "inbox", 2001, "Fresh off the wire");
    act(() => emitEnvelope("sync.completed", tick("work", ["Fresh off the wire"])));
    await waitFor(() => expect(shownRows()[0]).toBe(2001));
    expect(probe.state.messages.loadedGen).toBe(probe.state.messages.gen);
  });

  it("re-reads the open mailbox on a tick of its account that ingested elsewhere, and not on another account's", async () => {
    const probe = renderProbed();
    await shellReady();
    const before = listCalls();
    act(() => emitEnvelope("sync.completed", tick("home", ["Not yours"])));
    expect(probe.state.messages.loadedGen).toBe(probe.state.messages.gen);
    expect(listCalls()).toBe(before);
    arrive("work", "inbox", 2002, "Into the inbox while sent was synced");
    act(() => emitEnvelope("sync.completed", tick("work")));
    await waitFor(() => expect(shownRows()[0]).toBe(2002));
  });

  it("lands the second of two ticks whose reads overlap", async () => {
    const probe = renderProbed();
    await shellReady();
    let open!: () => void;
    mock.gates.set("list_messages", new Promise<void>((r) => (open = r)));
    const before = listCalls();
    act(() => emitEnvelope("sync.completed", tick("work")));
    // The first read is out, answered from the store before the second ingest.
    await waitFor(() => expect(listCalls()).toBe(before + 1));
    arrive("work", "inbox", 2003, "Second tick's mail");
    act(() => emitEnvelope("sync.completed", tick("work", ["Second tick's mail"])));
    await waitFor(() => expect(listCalls()).toBe(before + 2));
    await waitFor(() => expect(shownRows()[0]).toBe(2003));
    // The first, older answer lands last and does not take the row away.
    await act(async () => open());
    expect(shownRows()[0]).toBe(2003);
    expect(probe.state.messages.loadedGen).toBe(probe.state.messages.gen);
  });

  it("brings the tick's mail in when a search over the mailbox ends", async () => {
    const probe = renderProbed();
    await shellReady();
    act(() => probe.dispatch({ type: "search_local", query: "ledger" }));
    arrive("work", "inbox", 2004, "Arrived during a search");
    act(() => emitEnvelope("sync.completed", tick("work", ["Arrived during a search"])));
    await waitFor(() => expect(probe.state.messages.loadedGen).toBe(probe.state.messages.gen));
    act(() => probe.dispatch({ type: "exit_search" }));
    await waitFor(() => expect(shownRows()[0]).toBe(2004));
  });

  it("after a daemon restart, takes the new instance's ticks", async () => {
    const probe = renderProbed();
    await shellReady();
    mock.connection = { ...mock.connection, instance_id: "fixture-instance-2" } as typeof mock.connection;
    // The Rust layer re-bootstraps before it forwards the new instance's events.
    act(() => emit({ type: "rebootstrapped", cause: "instance_changed", bootstrap: { ...fixtures.bootstrap, instance_id: "fixture-instance-2" } }));
    await waitFor(() => expect(probe.state.messages.loadedGen).toBe(probe.state.messages.gen));
    arrive("work", "inbox", 2005, "From the new daemon");
    act(() => emitEnvelope("sync.completed", tick("work", ["From the new daemon"])));
    await waitFor(() => expect(shownRows()[0]).toBe(2005));
  });
});
