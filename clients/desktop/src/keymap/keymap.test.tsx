import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitEnvelope, emitMenu, fixtures, mock } from "@/test/tauri-mock";

function selectedSubject(): string | null {
  const row = document.querySelector('[role="option"][aria-selected="true"]');
  return row?.getAttribute("aria-label") ?? null;
}

describe("keyboard routing", () => {
  it("j and k move the selection in the list and the reader follows", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    expect(selectedSubject()).toMatch(/Quarterly ledger review/);
    await user.keyboard("j");
    expect(selectedSubject()).toMatch(/Angebot Dachsanierung/);
    await user.keyboard("k");
    expect(selectedSubject()).toMatch(/Quarterly ledger review/);
    const reader = screen.getByRole("complementary", { name: "Reader" });
    expect(await within(reader).findByRole("heading", { name: "Quarterly ledger review" })).toBeInTheDocument();
    expect(within(reader).getByTitle("Message body: Quarterly ledger review")).toHaveAttribute(
      "src",
      "mpmsg://localhost/work/1001",
    );
  });

  it("Tab and Shift+Tab cycle focus through sidebar, list and reader", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    const pane = () => document.activeElement?.closest("[data-pane]")?.getAttribute("data-pane");
    expect(pane()).toBe("list");
    await user.keyboard("{Tab}");
    expect(pane()).toBe("reader");
    await user.keyboard("{Tab}");
    expect(pane()).toBe("sidebar");
    await user.keyboard("{Tab}");
    expect(pane()).toBe("list");
    await user.keyboard("{Shift>}{Tab}{/Shift}");
    expect(pane()).toBe("sidebar");
  });

  it("the sidebar cursor moves with j and Enter opens the mailbox", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("gm");
    await user.keyboard("jj");
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("heading", { name: "Sent" })).toBeInTheDocument();
    expect(await screen.findByRole("listbox", { name: "Sent messages" })).toBeInTheDocument();
  });

  it("digits jump to a mailbox of the selected account", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("4");
    expect(await screen.findByRole("listbox", { name: "Archive messages" })).toBeInTheDocument();
  });

  it(": and Ctrl+p open the palette, ? the key help", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    expect(await screen.findByRole("dialog", { name: "Command palette" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await user.keyboard("{Control>}p{/Control}");
    expect(await screen.findByRole("dialog", { name: "Command palette" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await user.keyboard("?");
    expect(await screen.findByRole("dialog", { name: "Keys" })).toBeInTheDocument();
  });

  it("ignores keys while a text field has focus, except Escape", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("/");
    const input = screen.getByRole("searchbox", { name: /Filter this list/ });
    expect(input).toHaveFocus();
    await user.keyboard("j:");
    expect(selectedSubject()).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(input).toHaveValue("j:");
    await user.keyboard("{Escape}");
    expect(input).not.toHaveFocus();
  });

  it("z zooms the focused list pane, hiding the reader", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("z");
    expect(screen.queryByRole("complementary", { name: "Reader" })).toBeNull();
    await user.keyboard("z");
    expect(screen.getByRole("complementary", { name: "Reader" })).toBeInTheDocument();
  });

  it("a native menu item runs the same action as its key", async () => {
    renderApp();
    await shellReady();
    emitMenu("key_help");
    expect(await screen.findByRole("dialog", { name: "Keys" })).toBeInTheDocument();
  });

  it("a key for a later milestone says so instead of doing nothing", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("x");
    expect(await screen.findByText(/Send current draft \(approve \+ send\) arrives later in M3/)).toBeInTheDocument();
  });
});

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
const marked = () =>
  [...document.querySelectorAll('[role="option"][aria-selected="true"]')].map((e) => Number(e.getAttribute("data-row-id")));

/** The seeded hold fires, so `u` is toggle read again. */
function fireSeededHold() {
  act(() => emitEnvelope("send.hold_fired", { ...fixtures.bootstrap.snapshot.holds[0], remaining_secs: 0 }));
}

describe("mutation keys (the TUI's)", () => {
  it("a asks first, and y archives the cursor row", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("a");
    const dialog = await screen.findByRole("dialog", { name: "Archive this email?" });
    expect(dialog).toHaveTextContent("Quarterly ledger review");
    expect(callsOf("message_archive")).toEqual([]);
    await user.keyboard("y");
    await waitFor(() => expect(callsOf("message_archive")).toEqual([{ account: "work", row_ids: [1001] }]));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("n cancels the confirmation and nothing is called", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("d");
    expect(await screen.findByRole("dialog", { name: "Delete this email?" })).toBeInTheDocument();
    await user.keyboard("n");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(callsOf("message_delete")).toEqual([]);
  });

  it("v marks and steps, and d with Enter deletes the marked rows in list order", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("vv");
    expect(marked()).toEqual([1001, 1002]);
    await user.keyboard("d");
    expect(await screen.findByRole("dialog", { name: "Delete 2 emails?" })).toBeInTheDocument();
    await user.keyboard("{Enter}");
    await waitFor(() => expect(callsOf("message_delete")).toEqual([{ account: "work", row_ids: [1001, 1002] }]));
  });

  it("* flags the cursor row and unflags a flagged one", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j*");
    await waitFor(() => expect(callsOf("message_set_flag")).toEqual([{ account: "work", row_ids: [1001], flagged: true }]));
    await user.keyboard("jjj*");
    await waitFor(() => expect(callsOf("message_set_flag")[1]).toEqual({ account: "work", row_ids: [1004], flagged: false }));
  });

  it("over marks, * flags them all when any is unflagged, and the marks clear", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jjjvj");
    // 1003 is unflagged, 1004 flagged: flagging wins.
    await user.keyboard("{ArrowUp}{ArrowUp}");
    expect(marked()).toEqual([1003]);
    await user.keyboard("jvk");
    expect(marked()).toEqual([1003, 1004]);
    await user.keyboard("*");
    await waitFor(() => expect(callsOf("message_set_flag")).toEqual([{ account: "work", row_ids: [1003, 1004], flagged: true }]));
    expect(screen.queryByText(/marked/)).toBeNull();
  });

  it("u cancels a held send while one counts down, and toggles read otherwise", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("u");
    await waitFor(() => expect(callsOf("send_cancel_hold")).toEqual([{ operation_id: "fixture-hold-seed" }]));
    expect(callsOf("message_set_read")).toEqual([]);
    fireSeededHold();
    await user.keyboard("u");
    await waitFor(() => expect(callsOf("message_set_read")).toEqual([{ account: "work", row_ids: [1001], read: false }]));
  });

  it("X dismisses the newest notice, one per press", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j*");
    const area = screen.getByRole("region", { name: "Activity" });
    expect(await within(area).findByText("Flagged 1 message")).toBeInTheDocument();
    mock.failing.set("sync_trigger", { kind: "internal", message: "no route" });
    await user.keyboard("ss");
    expect(await within(area).findByRole("alert")).toHaveTextContent("Sync of work did not start: no route");
    await user.keyboard("X");
    expect(within(area).queryByRole("alert")).toBeNull();
    expect(within(area).getByText("Flagged 1 message")).toBeInTheDocument();
    await user.keyboard("X");
    expect(within(area).queryByText("Flagged 1 message")).toBeNull();
    // Nothing left: X is a no-op, and the held send stays.
    await user.keyboard("X");
    expect(within(area).getByRole("group", { name: /^Held send: / })).toBeInTheDocument();
  });

  it("M opens the mailbox picker, typing filters it and Enter moves", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jj");
    await user.keyboard("M");
    const picker = await screen.findByRole("dialog", { name: "Move the message to" });
    await user.keyboard("arch");
    await waitFor(() =>
      expect([...picker.querySelectorAll("[data-testid='move-destination']")].map((e) => e.getAttribute("data-slug"))).toEqual(["archive"]),
    );
    await user.keyboard("{Enter}");
    await waitFor(() =>
      expect(callsOf("message_move")).toEqual([{ account: "work", row_ids: [1002], destination: "archive" }]),
    );
  });

  it("Ctrl+a marks every row, and Escape clears the marks before the selection", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("{Control>}a{/Control}");
    expect(marked()).toHaveLength(8);
    await user.keyboard("{Escape}");
    expect(marked()).toEqual([1001]);
    await user.keyboard("{Escape}");
    expect(marked()).toEqual([]);
  });

  it("d on a draft discards it after the confirmation", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("2");
    await screen.findByRole("listbox", { name: "Drafts messages" });
    await user.keyboard("j");
    await user.keyboard("d");
    expect(await screen.findByRole("dialog", { name: "Delete this email?" })).toBeInTheDocument();
    await user.keyboard("y");
    await waitFor(() => expect(callsOf("draft_discard")).toEqual([{ account: "work", ids: ["angebot-antwort"] }]));
    expect(callsOf("message_delete")).toEqual([]);
  });

  it("ss and sS start a quick and a full sync of the selected account", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("ss");
    await user.keyboard("sS");
    await waitFor(() =>
      expect(callsOf("sync_trigger")).toEqual([
        { account: "work", mode: "quick" },
        { account: "work", mode: "full" },
      ]),
    );
  });

  it("the message keys do nothing from the sidebar, as the TUI's", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("gm");
    await user.keyboard("*");
    expect(callsOf("message_set_flag")).toEqual([]);
  });
});

/** A server-only hit, as `message.server_hit` carries it. */
function serverOnlyHit(subject: string) {
  return {
    account: "work",
    mailbox: "Archive",
    message_id: "<server-only@fixture.example>",
    row_id: null,
    selector: null,
    from: "Old Friend <old@example.com>",
    to: "me@example.com",
    cc: "cc@example.com",
    reply_to: "reply@example.com",
    bcc: null,
    subject,
    date_display: "Mon, 3 Mar 2025 09:00:00 +0100",
    date_sort: "2025-03-03T08:00:00",
    flags: { seen: true, answered: false, forwarded: false, flagged: false },
    has_attachments: false,
    is_invite: false,
    body_text: "The old thread.",
    html_body: null,
  };
}

async function drafts(user: ReturnType<typeof renderApp>["user"]) {
  await user.keyboard("2");
  await screen.findByRole("listbox", { name: "Drafts messages" });
}

describe("compose keys (the TUI's)", () => {
  it("cn opens the new-draft wizard for the shown account", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("cn");
    expect(await screen.findByRole("dialog", { name: "New draft" })).toBeInTheDocument();
    await waitFor(() => expect(callsOf("signature_list")).toEqual([{ account: "work" }]));
  });

  it("r and cr reply to the cursor row and open the draft in the editor, ca replies to all", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("r");
    await waitFor(() => expect(mock.editorOpens).toEqual(["/fixture/work/drafts/fixture-draft-1.md"]));
    await user.keyboard("cr");
    await user.keyboard("ca");
    await waitFor(() =>
      expect(callsOf("draft_reply")).toEqual([
        { account: "work", row_id: 1001, all: false, headers: null },
        { account: "work", row_id: 1001, all: false, headers: null },
        { account: "work", row_id: 1001, all: true, headers: null },
      ]),
    );
    await waitFor(() => expect(mock.editorOpens).toHaveLength(3));
    expect(callsOf("draft_create")).toEqual([]);
  });

  it("cf on a stored message asks for the recipients first, with the forward's subject", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jj");
    await user.keyboard("cf");
    const dialog = await screen.findByRole("dialog", { name: "Forward" });
    expect(within(dialog).getByLabelText("Subject")).toHaveValue("Fwd: Angebot Dachsanierung");
    expect(callsOf("draft_forward")).toEqual([]);
    await waitFor(() => expect(within(dialog).getByLabelText("To")).toHaveFocus());
    await user.keyboard("kim@example.com{Control>}{Enter}{/Control}");
    await waitFor(() =>
      expect(callsOf("draft_forward")).toEqual([
        { account: "work", row_id: 1002, headers: { to: "kim@example.com", cc: "", bcc: "", subject: "Fwd: Angebot Dachsanierung" } },
      ]),
    );
    await waitFor(() => expect(mock.editorOpens).toHaveLength(1));
  });

  it("on a server-only hit, r, ca and cf build the draft from the hit's own headers", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(screen.getByRole("searchbox", { name: /Filter this list/ }));
    await user.keyboard("thread{Shift>}{Enter}{/Shift}");
    await waitFor(() => expect(callsOf("search_server_start")).toHaveLength(1));
    act(() => emitEnvelope("message.server_hit", { operation_id: "op-1", hit: serverOnlyHit("The old thread") }));
    const hit = await screen.findByRole("option", { name: /The old thread/ });
    await user.click(hit);
    expect(hit).toHaveAttribute("aria-selected", "true");
    const reader = screen.getByRole("complementary", { name: "Reader" });
    expect(await within(reader).findByText(/This message is on the server only/)).toBeInTheDocument();
    await user.keyboard("r");
    await user.keyboard("ca");
    await user.keyboard("cf");
    const message = {
      from: "Old Friend <old@example.com>",
      to: "me@example.com",
      cc: "cc@example.com",
      reply_to: "reply@example.com",
      subject: "The old thread",
      message_id: "<server-only@fixture.example>",
      date_display: "Mon, 3 Mar 2025 09:00:00 +0100",
      body_text: "The old thread.",
      html_body: null,
    };
    await waitFor(() =>
      expect(callsOf("draft_from_message")).toEqual([
        { account: "work", kind: "reply", message },
        { account: "work", kind: "reply_all", message },
        { account: "work", kind: "forward", message },
      ]),
    );
    expect(callsOf("draft_reply")).toEqual([]);
    expect(callsOf("draft_forward")).toEqual([]);
    expect(screen.queryByRole("dialog", { name: "Forward" })).toBeNull();
    await waitFor(() => expect(mock.editorOpens).toHaveLength(3));
  });

  it("e on a draft resolves its path and opens the editor; on a message it opens the reader", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("e");
    const pane = () => document.activeElement?.closest("[data-pane]")?.getAttribute("data-pane");
    await waitFor(() => expect(pane()).toBe("reader"));
    expect(callsOf("draft_path")).toEqual([]);
    expect(mock.editorOpens).toEqual([]);

    await user.keyboard("{Escape}");
    await drafts(user);
    await user.keyboard("j");
    await user.keyboard("e");
    await waitFor(() => expect(mock.editorOpens).toEqual(["/fixture/work/drafts/angebot-antwort.md"]));
    expect(callsOf("draft_path")).toEqual([{ account: "work", id: "angebot-antwort" }]);
  });

  it("ce, cA and cD act in Drafts only, and say so elsewhere", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("cA");
    expect(await screen.findByText("Approve (c A) is only available in Drafts")).toBeInTheDocument();
    await user.keyboard("ce");
    expect(await screen.findByText("Edit recipients (c e) is only available in Drafts")).toBeInTheDocument();
    expect(callsOf("draft_approve")).toEqual([]);
    expect(callsOf("draft_preview")).toEqual([]);
  });

  it("cA approves the cursor draft and cD puts it back", async () => {
    const { user } = renderApp();
    await shellReady();
    await drafts(user);
    await user.keyboard("j");
    await user.keyboard("cA");
    await waitFor(() => expect(callsOf("draft_approve")).toEqual([{ account: "work", ids: ["angebot-antwort"] }]));
    const row = () => document.querySelector('[data-draft-id="angebot-antwort"]');
    await waitFor(() => expect(row()?.querySelector('[data-slot="draft-status"]')).toHaveTextContent("approved"));
    await user.keyboard("cD");
    await waitFor(() => expect(callsOf("draft_demote")).toEqual([{ account: "work", ids: ["angebot-antwort"] }]));
    await waitFor(() => expect(row()?.querySelector('[data-slot="draft-status"]')).toHaveTextContent("draft"));
  });

  it("cA over marked drafts asks first, with the TUI's words, and approves the batch", async () => {
    const { user } = renderApp();
    await shellReady();
    await drafts(user);
    await user.keyboard("j");
    await user.keyboard("{Control>}a{/Control}");
    await user.keyboard("cA");
    expect(await screen.findByRole("dialog", { name: "Approve 2 drafts?" })).toBeInTheDocument();
    expect(callsOf("draft_approve")).toEqual([]);
    await user.keyboard("y");
    await waitFor(() =>
      expect(callsOf("draft_approve")).toEqual([{ account: "work", ids: ["angebot-antwort", "offsite-note"] }]),
    );
  });

  it("ce opens the recipients dialog filled from the draft file", async () => {
    const { user } = renderApp();
    await shellReady();
    await drafts(user);
    await user.keyboard("j");
    await user.keyboard("ce");
    const dialog = await screen.findByRole("dialog", { name: "Edit recipients" });
    expect(within(dialog).getByLabelText("To")).toHaveValue("robin@example.com");
    expect(within(dialog).getByLabelText("Subject")).toHaveValue("Re: Angebot Dachsanierung");
    expect(callsOf("draft_preview")).toContainEqual({ account: "work", id: "angebot-antwort" });
  });

  it("the compose row keys do nothing from the sidebar", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("gm");
    await user.keyboard("cr");
    await user.keyboard("r");
    expect(callsOf("draft_reply")).toEqual([]);
  });
});
