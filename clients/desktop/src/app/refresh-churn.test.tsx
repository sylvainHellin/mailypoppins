// PERSO-272: the window blinks, seemingly when drafts sync with the server.
// A daemon event that only says "read this again" (draft.changed,
// draft.invalid, sync.completed, account.state_changed) must not blank what
// the window shows: the reader keeps its content and its frame, the list
// keeps its rows, and no loading skeleton flashes while the re-read is out.

import { StrictMode } from "react";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { AppShell } from "@/components/shell/AppShell";
import { TooltipProvider } from "@/components/ui/tooltip";
import { StoreProvider, useAppState, useDispatch } from "@/app/store";
import type { Action } from "@/app/reducer";
import { DEFAULT_PREFS, type AppState } from "@/app/state";
import { PREFS_KEY } from "@/app/prefs";
import { forgetTexts } from "@/app/readerMode";
import { forgetReads } from "@/lib/inflight";
import { setWidth } from "@/test/setup";
import { CANNED_THREAD, emitEnvelope, mock, resetMock } from "@/test/tauri-mock";

/** The skeletons a pane shows in place of its content while it has none. */
const SKELETONS = ['[aria-label="Loading the draft"]', '[aria-label="Loading message"]', '[aria-label="Loading messages"]'].join(",");

/**
 * Every loading skeleton mounted from now on, even one that is gone again
 * before the test looks: a blink is a skeleton that shows for a frame.
 */
function watchSkeletons() {
  const seen: string[] = [];
  const take = (records: MutationRecord[]) => {
    for (const r of records) {
      for (const n of r.addedNodes) {
        if (!(n instanceof Element)) continue;
        const hits = n.matches(SKELETONS) ? [n] : [...n.querySelectorAll(SKELETONS)];
        for (const h of hits) seen.push(h.getAttribute("aria-label") ?? "?");
      }
    }
  };
  const observer = new MutationObserver(take);
  observer.observe(document.body, { childList: true, subtree: true });
  return {
    seen: () => {
      take(observer.takeRecords());
      return seen;
    },
  };
}

/** The app as main.tsx renders it, with a probe on the model. */
function renderProbed(opts: { conversations?: boolean } = {}) {
  resetMock();
  forgetTexts();
  forgetReads();
  if (opts.conversations) localStorage.setItem(PREFS_KEY, JSON.stringify(DEFAULT_PREFS));
  setWidth(1400);
  const user = userEvent.setup();
  const probe = {} as { state: AppState; dispatch: (a: Action) => void };
  function Probe() {
    probe.state = useAppState();
    probe.dispatch = useDispatch();
    return null;
  }
  render(
    <StrictMode>
      <StoreProvider>
        <TooltipProvider>
          <AppShell />
          <Probe />
        </TooltipProvider>
      </StoreProvider>
    </StrictMode>,
  );
  return { user, probe };
}

const reader = () => screen.getByRole("complementary", { name: "Reader" });
const draftBody = () => reader().querySelector('[data-slot="draft-body"]');
const readerFrame = () => reader().querySelector('iframe[data-slot="reader-frame"]');
const selectedRow = () => document.querySelector('[role="option"][aria-selected="true"]');
const listCalls = () => mock.calls.filter((c) => c.cmd === "list_messages").length;

/** Emit `kind`, then wait until the shown list was read again and every effect that follows ran. */
async function refresh(probe: { state: AppState }, kind: string, payload: unknown) {
  const calls = listCalls();
  const gen = probe.state.messages.gen;
  act(() => emitEnvelope(kind, payload));
  await waitFor(() => expect(listCalls()).toBeGreaterThan(calls));
  await waitFor(() => expect(probe.state.messages.loadedGen).toBeGreaterThan(gen));
  await act(async () => {
    await new Promise((r) => setTimeout(r, 30));
  });
}

const tick = (account: string) => ({
  account,
  severity: "ok",
  saved: 0,
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
  new_inbox_mail: [],
});

/** What the watcher publishes when the other fixture draft is written, by a save or by a server sync. */
const otherDraftChanged = {
  account: "work",
  id: "offsite-note",
  path: "/fixture/work/drafts/offsite-note.md",
  to: null,
  subject: null,
  status: "draft",
  valid: true,
  ready: false,
};

/** The work account's Drafts, its first draft's Markdown shown. */
async function openDraft(user: ReturnType<typeof userEvent.setup>) {
  await screen.findByRole("listbox", {}, { timeout: 3000 });
  await user.keyboard("2");
  await screen.findByRole("listbox", { name: "Drafts messages" });
  await within(reader()).findByRole("article", { name: "Draft: Re: Angebot Dachsanierung" });
  await waitFor(() => expect(draftBody()).toHaveTextContent("vielen Dank für das Angebot"));
}

describe("a refresh event keeps the shown draft (PERSO-272)", () => {
  it("a draft.changed of another draft does not blank the shown draft's preview", async () => {
    const { user, probe } = renderProbed();
    await openDraft(user);
    const watch = watchSkeletons();
    await refresh(probe, "draft.changed", otherDraftChanged);
    expect(watch.seen()).toEqual([]);
    expect(draftBody()).toHaveTextContent("vielen Dank für das Angebot");
  });

  it("a draft.invalid of another draft does not blank the shown draft's preview", async () => {
    const { user, probe } = renderProbed();
    await openDraft(user);
    const watch = watchSkeletons();
    await refresh(probe, "draft.invalid", {
      account: "work",
      id: "offsite-note",
      path: "/fixture/work/drafts/offsite-note.md",
      diagnostics: [{ line: 1, message: "did not find expected key" }],
    });
    expect(watch.seen()).toEqual([]);
    expect(draftBody()).toHaveTextContent("vielen Dank für das Angebot");
  });

  it("a sync.completed of the account does not blank the shown draft's preview", async () => {
    const { user, probe } = renderProbed();
    await openDraft(user);
    const watch = watchSkeletons();
    await refresh(probe, "sync.completed", tick("work"));
    expect(watch.seen()).toEqual([]);
    expect(draftBody()).toHaveTextContent("vielen Dank für das Angebot");
  });

  it("an account.state_changed of the account does not blank the shown draft's preview", async () => {
    const { user, probe } = renderProbed();
    await openDraft(user);
    const watch = watchSkeletons();
    await refresh(probe, "account.state_changed", { account: "work", state: "ready" });
    expect(watch.seen()).toEqual([]);
    expect(draftBody()).toHaveTextContent("vielen Dank für das Angebot");
  });

  it("a draft.changed of the shown draft keeps its preview up while the new one is read", async () => {
    const { user, probe } = renderProbed();
    await openDraft(user);
    const watch = watchSkeletons();
    let open!: () => void;
    mock.gates.set("draft_preview", new Promise<void>((r) => (open = r)));
    await refresh(probe, "draft.changed", {
      account: "work",
      id: "angebot-antwort",
      path: "/fixture/work/drafts/angebot-antwort.md",
      to: "robin@example.com",
      subject: "Re: Angebot Dachsanierung",
      status: "draft",
      valid: true,
      ready: true,
    });
    // The re-read is held: what was shown stays until its answer lands.
    expect(draftBody()).toHaveTextContent("vielen Dank für das Angebot");
    await act(async () => open());
    await waitFor(() => expect(draftBody()).toHaveTextContent("vielen Dank für das Angebot"));
    expect(watch.seen()).toEqual([]);
  });

  it("a draft.changed of another draft does not reload the shown draft's HTML frame", async () => {
    const { user, probe } = renderProbed();
    await openDraft(user);
    await user.keyboard("tt");
    await waitFor(() => expect(readerFrame()).not.toBeNull());
    const before = readerFrame();
    await refresh(probe, "draft.changed", otherDraftChanged);
    expect(readerFrame()).toBe(before);
  });

  it("a draft.changed of the shown draft with the version it carried before neither re-reads it nor reloads its frame; a new version does", async () => {
    const { user, probe } = renderProbed();
    await openDraft(user);
    await user.keyboard("tt");
    await waitFor(() => expect(readerFrame()).not.toBeNull());
    const previews = () => mock.calls.filter((c) => c.cmd === "draft_preview").length;
    const shown = (version: string) => ({
      account: "work",
      id: "angebot-antwort",
      path: "/fixture/work/drafts/angebot-antwort.md",
      to: "robin@example.com",
      subject: "Re: Angebot Dachsanierung",
      status: "draft",
      valid: true,
      ready: true,
      version,
    });
    // A save: the first version the window sees of the file.
    let reads = previews();
    let before = readerFrame();
    await refresh(probe, "draft.changed", shown("v1"));
    await waitFor(() => expect(previews()).toBeGreaterThan(reads));
    await waitFor(() => expect(readerFrame()).not.toBe(before));
    // Its push lands: the row's server state moved, the file did not.
    reads = previews();
    before = readerFrame();
    await refresh(probe, "draft.changed", { ...shown("v1"), server: { state: "synced", synced_at: "2026-11-06T12:00:00Z" } });
    expect(previews()).toBe(reads);
    expect(readerFrame()).toBe(before);
    // Another save.
    await refresh(probe, "draft.changed", shown("v2"));
    await waitFor(() => expect(previews()).toBeGreaterThan(reads));
    await waitFor(() => expect(readerFrame()).not.toBe(before));
    expect(readerFrame()).not.toBeNull();
  });

  it("a sync.completed of the account does not reload the shown draft's HTML frame", async () => {
    const { user, probe } = renderProbed();
    await openDraft(user);
    await user.keyboard("tt");
    await waitFor(() => expect(readerFrame()).not.toBeNull());
    const before = readerFrame();
    await refresh(probe, "sync.completed", tick("work"));
    expect(readerFrame()).toBe(before);
  });
});

describe("a refresh event keeps a conversation's shown draft member (PERSO-272)", () => {
  /** The canned conversation expanded, its draft member shown through `ComposeSummary`. */
  async function openMember(user: ReturnType<typeof userEvent.setup>) {
    const header = () => document.querySelector<HTMLElement>(`[role="option"][data-thread-id="${CANNED_THREAD}"]`);
    await waitFor(() => expect(header()).not.toBeNull(), { timeout: 3000 });
    await user.click(within(header()!).getByRole("button", { name: "Expand conversation (l)" }));
    const member = await waitFor(() => {
      const m = document.querySelector<HTMLElement>('[role="option"][data-member="draft:offsite-note"]');
      expect(m).not.toBeNull();
      return m!;
    });
    await user.click(member);
    await waitFor(() => expect(draftBody()).toHaveTextContent("Notes for the offsite agenda."));
  }

  const cases: { name: string; kind: string; payload: unknown }[] = [
    {
      name: "a draft.changed of another draft",
      kind: "draft.changed",
      payload: { ...otherDraftChanged, id: "angebot-antwort", path: "/fixture/work/drafts/angebot-antwort.md" },
    },
    { name: "a sync.completed of the account", kind: "sync.completed", payload: tick("work") },
    { name: "an account.state_changed of the account", kind: "account.state_changed", payload: { account: "work", state: "ready" } },
  ];

  it("a sync.completed of the account reads the member's draft again no more", async () => {
    const { user, probe } = renderProbed({ conversations: true });
    await openMember(user);
    const previews = () => mock.calls.filter((c) => c.cmd === "draft_preview").length;
    const reads = previews();
    await refresh(probe, "sync.completed", tick("work"));
    await refresh(probe, "sync.completed", tick("work"));
    expect(previews()).toBe(reads);
    expect(draftBody()).toHaveTextContent("Notes for the offsite agenda.");
  });

  for (const c of cases) {
    it(`${c.name} does not blank the member's preview`, async () => {
      const { user, probe } = renderProbed({ conversations: true });
      await openMember(user);
      const article = within(reader()).getByRole("article", { name: /^Draft: / });
      const watch = watchSkeletons();
      await refresh(probe, c.kind, c.payload);
      expect(watch.seen()).toEqual([]);
      expect(within(reader()).getByRole("article", { name: /^Draft: / })).toBe(article);
      expect(draftBody()).toHaveTextContent("Notes for the offsite agenda.");
    });
  }
});

describe("a refresh event keeps the shown message (PERSO-272)", () => {
  /** The work inbox's first message open in the reader. */
  async function openMessage(user: ReturnType<typeof userEvent.setup>) {
    await screen.findByRole("listbox", {}, { timeout: 3000 });
    await user.keyboard("j");
    await within(reader()).findByRole("toolbar", { name: "Message actions" });
    await waitFor(() => expect(readerFrame()).not.toBeNull());
  }

  const cases: { name: string; conversations: boolean; kind: string; payload: unknown }[] = [
    { name: "a draft.changed, conversation list", conversations: true, kind: "draft.changed", payload: otherDraftChanged },
    { name: "a sync.completed, conversation list", conversations: true, kind: "sync.completed", payload: tick("work") },
    { name: "an account.state_changed, conversation list", conversations: true, kind: "account.state_changed", payload: { account: "work", state: "ready" } },
    { name: "a sync.completed, flat list", conversations: false, kind: "sync.completed", payload: tick("work") },
    { name: "an account.state_changed, flat list", conversations: false, kind: "account.state_changed", payload: { account: "work", state: "ready" } },
  ];

  for (const c of cases) {
    it(`${c.name}: the reader keeps its content and frame, and the list its rows`, async () => {
      const { user, probe } = renderProbed({ conversations: c.conversations });
      await openMessage(user);
      const frame = readerFrame();
      const row = selectedRow();
      const metaCalls = mock.calls.filter((x) => x.cmd === "message_html_meta").length;
      expect(row).not.toBeNull();
      const watch = watchSkeletons();
      await refresh(probe, c.kind, c.payload);
      expect(watch.seen()).toEqual([]);
      expect(readerFrame()).toBe(frame);
      expect(selectedRow()).toBe(row);
      expect(within(reader()).getByRole("toolbar", { name: "Message actions" })).toBeInTheDocument();
      // The same message is not read again.
      expect(mock.calls.filter((x) => x.cmd === "message_html_meta").length).toBe(metaCalls);
    });
  }
});

describe("an equal reloaded list (PERSO-272)", () => {
  it("keeps the list it replaces, so no row re-renders", async () => {
    const { probe } = renderProbed();
    await screen.findByRole("listbox", {}, { timeout: 3000 });
    const before = probe.state.messages.data;
    await refresh(probe, "sync.completed", tick("work"));
    expect(probe.state.messages.data).toBe(before);
  });
});
