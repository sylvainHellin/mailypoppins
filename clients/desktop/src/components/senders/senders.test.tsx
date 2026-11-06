import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitEnvelope, mock } from "@/test/tauri-mock";
import { paletteEntries } from "@/keymap/catalog";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);

/** Give `work` a Junk folder, so a block moves its Inbox mail there. */
function workHasJunk() {
  mock.bootstrap.snapshot.mailboxes.work.push({ role: "junk", slug: "junk", label: "Junk", total: 0, unread: 0, badge: 0 } as never);
}

const SKIPPED_HOME = "home has no Junk folder; its Inbox keeps the sender's mail";

async function chooserOnFirstRow(before?: () => void) {
  const { user } = renderApp(1400, before);
  await shellReady();
  await user.keyboard("j");
  await screen.findByRole("toolbar", { name: "Message actions" });
  await user.keyboard("B");
  const dialog = await screen.findByRole("dialog", { name: "Block sender" });
  return { user, dialog };
}

const choices = (dialog: HTMLElement) =>
  within(dialog)
    .getAllByRole("radio")
    .map((r) => [r.getAttribute("data-choice"), r.getAttribute("aria-label"), r.getAttribute("aria-checked")]);

async function openBlocked(entries: string[]) {
  const { user } = renderApp(1400, () => {
    mock.blocked = entries;
  });
  await shellReady();
  await user.keyboard("sb");
  const dialog = await screen.findByRole("dialog", { name: "Blocked senders" });
  const list = within(dialog).getByRole("listbox", { name: "Blocked senders" });
  return { user, dialog, list };
}

const listed = (list: HTMLElement) => within(list).queryAllByRole("option").map((o) => o.getAttribute("data-blocked"));
const cursor = (list: HTMLElement) => within(list).queryByRole("option", { selected: true })?.getAttribute("data-blocked") ?? null;

describe("blocking a sender", () => {
  it("B opens the chooser on the sender's address and its domain; Escape blocks nothing", async () => {
    const { user, dialog } = await chooserOnFirstRow();
    expect(choices(dialog)).toEqual([
      ["address", "ivana@example.com, this address", "true"],
      ["domain", "@example.com, everyone at this domain", "false"],
    ]);
    expect(dialog).toHaveTextContent("Its mail in every account's Inbox moves to Junk, now and as it arrives.");
    expect(within(dialog).getByRole("button", { name: /Block ivana@example.com/ })).toBeEnabled();
    await user.keyboard("j");
    expect(within(dialog).getByRole("button", { name: /Block @example.com/ })).toBeInTheDocument();
    await user.keyboard("k");
    expect(within(dialog).getByRole("button", { name: /Block ivana@example.com/ })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Block sender" })).toBeNull());
    expect(callsOf("sender_block")).toEqual([]);
  });

  it("d picks the domain and Enter blocks it: the Inbox mail moves to Junk and the notice counts it and names the account with no Junk folder", async () => {
    const { user, dialog } = await chooserOnFirstRow(workHasJunk);
    await user.keyboard("d");
    expect(choices(dialog).map((c) => c[2])).toEqual(["false", "true"]);
    await user.keyboard("{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Block sender" })).toBeNull());
    expect(callsOf("sender_block")).toEqual([{ address: "@example.com" }]);
    expect(
      await screen.findByText(`Blocked @example.com; 5 messages moved to Junk (work 5); ${SKIPPED_HOME}`),
    ).toBeInTheDocument();
    expect(mock.blocked).toEqual(["@example.com"]);
    expect(mock.rows.work.junk.map((r) => r.id).sort()).toEqual([1001, 1002, 1004, 1007, 1008]);
    // The list reads the Inbox again: only the senders at other domains are left.
    const inbox = await screen.findByRole("listbox", { name: /Inbox/ });
    await waitFor(() => expect(within(inbox).queryByText("Quarterly ledger review")).toBeNull());
    expect(within(inbox).getByText("Re: BIM model handover")).toBeInTheDocument();
  });

  it("the Block button blocks the address, and a sender already blocked says so", async () => {
    const { user, dialog } = await chooserOnFirstRow(() => {
      mock.blocked = ["ivana@example.com"];
    });
    await user.click(within(dialog).getByRole("button", { name: /Block ivana@example.com/ }));
    expect(callsOf("sender_block")).toEqual([{ address: "ivana@example.com" }]);
    expect(await screen.findByText(/^ivana@example.com was already blocked; no Inbox mail from it; /)).toBeInTheDocument();
  });

  it("a refused block keeps the chooser open with the daemon's sentence", async () => {
    const { user, dialog } = await chooserOnFirstRow(() => {
      mock.failing.set("sender_block", { kind: "protocol", message: "the daemon refused it", code: -32602 });
    });
    await user.keyboard("{Enter}");
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("Cannot block ivana@example.com: the daemon refused it");
    expect(screen.getByRole("dialog", { name: "Block sender" })).toBeInTheDocument();
  });

  it("the palette's row and the reader's button open the chooser; with no message the palette says so", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    await screen.findByRole("dialog", { name: "Command palette" });
    await user.keyboard("Block the sender{Enter}");
    expect(await screen.findByText("Select a message first")).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "Block sender" })).toBeNull();
    await user.keyboard("j");
    const toolbar = await screen.findByRole("toolbar", { name: "Message actions" });
    await user.keyboard(":");
    await screen.findByRole("dialog", { name: "Command palette" });
    await user.keyboard("Block the sender{Enter}");
    const dialog = await screen.findByRole("dialog", { name: "Block sender" });
    expect(choices(dialog)[0][1]).toContain("ivana@example.com");
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Block sender" })).toBeNull());
    await user.click(within(toolbar).getByRole("button", { name: "Block sender" }));
    expect(await screen.findByRole("dialog", { name: "Block sender" })).toBeInTheDocument();
  });

  it("both rows are in the palette under the TUI's keys", () => {
    expect(paletteEntries().find((e) => e.id === "block_sender")).toMatchObject({
      section: "MESSAGE (list, headers, body)",
      label: "Block the sender (address or @domain), its mail to Junk",
      keys: ["B"],
      badge: null,
    });
    expect(paletteEntries().find((e) => e.id === "blocked_senders")).toMatchObject({
      section: "GLOBAL",
      label: "Blocked senders (unblock one)",
      keys: ["sb"],
      badge: null,
    });
  });
});

describe("the Blocked senders dialog", () => {
  it("sb lists the blocked senders; u unblocks the cursor entry and the list follows", async () => {
    const { user, list } = await openBlocked(["@spam.example", "offers@shop.example"]);
    await waitFor(() => expect(listed(list)).toEqual(["@spam.example", "offers@shop.example"]));
    expect(callsOf("sender_list")).toEqual([undefined]);
    expect(cursor(list)).toBe("@spam.example");
    await user.keyboard("j");
    expect(cursor(list)).toBe("offers@shop.example");
    await user.keyboard("u");
    expect(callsOf("sender_unblock")).toEqual([{ address: "offers@shop.example" }]);
    await waitFor(() => expect(listed(list)).toEqual(["@spam.example"]));
    expect(cursor(list)).toBe("@spam.example");
    expect(await screen.findByText("Unblocked offers@shop.example; its mail already in Junk stays there")).toBeInTheDocument();
    await user.keyboard("u");
    await waitFor(() => expect(listed(list)).toEqual([]));
    expect(list).toHaveTextContent("No blocked senders; B on a message blocks its sender");
    expect(mock.blocked).toEqual([]);
  });

  it("stays live: another client's block or unblock replaces the open list, the cursor kept on its entry", async () => {
    const { user, list } = await openBlocked(["b@x.example", "c@x.example"]);
    await waitFor(() => expect(listed(list)).toEqual(["b@x.example", "c@x.example"]));
    await user.keyboard("j");
    act(() => emitEnvelope("sender.blocklist_changed", { entries: ["a@x.example", "b@x.example", "c@x.example"] }));
    await waitFor(() => expect(listed(list)).toEqual(["a@x.example", "b@x.example", "c@x.example"]));
    expect(cursor(list)).toBe("c@x.example");
    act(() => emitEnvelope("sender.blocklist_changed", { entries: ["a@x.example"] }));
    await waitFor(() => expect(listed(list)).toEqual(["a@x.example"]));
    expect(cursor(list)).toBe("a@x.example");
    // The event is the list: nothing is read again.
    expect(callsOf("sender_list").length).toBe(1);
  });

  it("the palette's row opens it from another view, and q closes it", async () => {
    const { user } = renderApp(1400, () => {
      mock.blocked = ["@spam.example"];
    });
    await shellReady();
    await user.keyboard(" c");
    await user.keyboard(":");
    await screen.findByRole("dialog", { name: "Command palette" });
    await user.keyboard("Blocked senders{Enter}");
    const dialog = await screen.findByRole("dialog", { name: "Blocked senders" });
    await waitFor(() => expect(listed(within(dialog).getByRole("listbox"))).toEqual(["@spam.example"]));
    await user.keyboard("q");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Blocked senders" })).toBeNull());
  });
});
