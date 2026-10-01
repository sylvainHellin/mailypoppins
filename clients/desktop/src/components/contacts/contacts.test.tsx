import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { mock, settleRebuild } from "@/test/tauri-mock";
import { CONTACTS_SEARCH_ID } from "@/app/contacts";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
const contacts = () => screen.findByRole("listbox", { name: "Contacts" });
const cursorRow = () => document.querySelector('[role="option"][aria-selected="true"]')?.getAttribute("data-address");
const field = () => document.getElementById(CONTACTS_SEARCH_ID) as HTMLInputElement;
const status = () => document.querySelector('[data-slot="contacts-status"]');

async function openContacts(before?: () => void) {
  const r = await showContacts(before);
  return { ...r, list: await contacts() };
}

async function showContacts(before?: () => void) {
  const r = renderApp(1400, before);
  await shellReady();
  // user-event installs its own clipboard; the view's copies go to this one.
  const writeText = vi.fn(async () => {});
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
  await r.user.keyboard(" c");
  await screen.findByRole("region", { name: "Contacts" });
  return { ...r, writeText };
}

describe("the Contacts view", () => {
  it("lists the selected account's ranked contacts in a listbox named Contacts, with their counts and scores", async () => {
    const { list } = await openContacts();
    expect(callsOf("contact_search")).toEqual([{ account: "work", query: "", limit: 1000 }]);
    const rows = within(list).getAllByRole("option");
    expect(rows).toHaveLength(25);
    expect(rows[0]).toHaveAttribute("aria-selected", "true");
    expect(rows[0]).toHaveAttribute("tabindex", "0");
    expect(rows.filter((r) => r.getAttribute("tabindex") === "0")).toHaveLength(1);
    expect(rows[0]).toHaveAttribute("aria-posinset", "1");
    expect(rows[0]).toHaveAttribute("aria-setsize", "25");
    expect(within(list).getByRole("option", { name: "Robin Meyer, robin@example.com, to 42, cc 3, received 57" })).toBeInTheDocument();
    expect(within(list).getByRole("option", { name: "Doe, Jane, jane.doe@example.com, to 31, cc 5, received 40" })).toBeInTheDocument();
    // A contact no message named shows its address alone.
    expect(within(list).getByRole("option", { name: "ops@example.com, to 8, cc 3, received 5" })).toBeInTheDocument();
    expect(rows[0].querySelector('[data-slot="contact-score"]')).toHaveTextContent("1000");
    expect(status()).toHaveTextContent("work: 25 contacts");
  });

  it("j, k, G and gg move the cursor, and each open reads the list again", async () => {
    const { user } = await openContacts();
    await user.keyboard("j");
    expect(cursorRow()).toBe("jane.doe@example.com");
    await user.keyboard("G");
    expect(cursorRow()).toBe("zoe@example.com");
    await user.keyboard("k");
    expect(cursorRow()).toBe("ben.lee@example.com");
    await user.keyboard("gg");
    expect(cursorRow()).toBe("robin@example.com");
    expect(document.activeElement).toHaveAttribute("data-address", "robin@example.com");
    await user.keyboard(" m");
    await user.keyboard(" c");
    await waitFor(() => expect(callsOf("contact_search")).toHaveLength(2));
  });

  it("/ focuses the search field, typing asks once per pause, and Escape leaves it with the query kept", async () => {
    const { user } = await openContacts();
    await user.keyboard("/");
    expect(document.activeElement).toBe(field());
    await user.keyboard("doe");
    await waitFor(() => expect(callsOf("contact_search")).toHaveLength(2));
    expect(callsOf("contact_search")[1]).toEqual({ account: "work", query: "doe", limit: 1000 });
    const list = await contacts();
    await waitFor(() => expect(within(list).getAllByRole("option")).toHaveLength(1));
    await new Promise((r) => setTimeout(r, 300));
    expect(callsOf("contact_search")).toHaveLength(2);
    expect(status()).toHaveTextContent("work: 1 contact matching doe");
    await user.keyboard("{Escape}");
    expect(document.activeElement).not.toBe(field());
    expect(field()).toHaveValue("doe");
    expect(document.activeElement).toHaveAttribute("data-address", "jane.doe@example.com");
    // The list has the keys again: `c` copies instead of typing.
    await user.keyboard("/{Backspace}{Backspace}{Backspace}zzz");
    await waitFor(() => expect(callsOf("contact_search")).toHaveLength(3));
    expect(callsOf("contact_search")[2]).toMatchObject({ query: "zzz" });
    expect(await screen.findByText("No matching contacts.")).toBeInTheDocument();
  });

  it("c copies the address through the clipboard and arms no c family key", async () => {
    const { user, writeText } = await openContacts();
    await user.keyboard("j");
    await user.keyboard("c");
    expect(writeText).toHaveBeenCalledWith("jane.doe@example.com");
    expect(await screen.findByText("Copied jane.doe@example.com")).toBeInTheDocument();
    // Mail's `ca` replies to all; here `a` after `c` does nothing.
    await user.keyboard("a");
    expect(callsOf("draft_reply")).toEqual([]);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("Enter and n open the new-draft wizard with the contact in To, a comma name quoted, the focus in Subject", async () => {
    const { user } = await openContacts();
    await user.keyboard("j{Enter}");
    let wizard = await screen.findByRole("dialog", { name: "New draft" });
    expect(within(wizard).getByLabelText("To")).toHaveValue('"Doe, Jane" <jane.doe@example.com>');
    await waitFor(() => expect(within(wizard).getByLabelText("Subject")).toHaveFocus());
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    await user.keyboard("gg");
    await user.keyboard("n");
    wizard = await screen.findByRole("dialog", { name: "New draft" });
    expect(within(wizard).getByLabelText("To")).toHaveValue("Robin Meyer <robin@example.com>");
    await user.type(within(wizard).getByLabelText("Subject"), "Hello{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(callsOf("draft_create")).toHaveLength(1));
    expect(callsOf("draft_create")[0]).toMatchObject({ account: "work", headers: { to: "Robin Meyer <robin@example.com>", subject: "Hello" } });
  });

  it("v writes a vCard draft to the contact and opens it in the editor", async () => {
    const { user } = await openContacts();
    await user.keyboard("jv");
    await waitFor(() => expect(callsOf("contact_vcard_draft")).toHaveLength(1));
    const args = callsOf("contact_vcard_draft")[0]!;
    expect(args).toMatchObject({ account: "work", address: "jane.doe@example.com", display_name: "Doe, Jane" });
    expect(String(args.name)).toMatch(/^draft-\d{4}-\d{2}-\d{2}-\d{6}-contact-doe-jane$/);
    await waitFor(() => expect(mock.editorOpens).toHaveLength(1));
    expect(mock.editorOpens[0]).toBe(`/fixture/work/drafts/${String(args.name)}.md`);
    expect(mock.vcards).toEqual([{ account: "work", id: expect.any(String), vcf: "/fixture/work/drafts/_vcards/doe-jane.vcf" }]);
    expect(await screen.findByText(/vCard draft: "Doe, Jane" <jane.doe@example.com>/)).toBeInTheDocument();
  });

  it("r rebuilds the index with the pending state in the header, then says the TUI's four outcomes", async () => {
    const { user } = await openContacts();
    await user.keyboard("r");
    await waitFor(() => expect(callsOf("contact_rebuild")).toEqual([{ account: "work" }]));
    await waitFor(() => expect(status()).toHaveTextContent("Rebuilding the contact index of work…"));
    expect(screen.getByRole("button", { name: "Rebuild index" })).toHaveAttribute("aria-busy", "true");
    // One at a time.
    await user.keyboard("r");
    expect(await screen.findByText("The contact index of work is already being rebuilt")).toBeInTheDocument();
    expect(callsOf("contact_rebuild")).toHaveLength(1);
    expect(callsOf("draft_reply")).toEqual([]);

    settleRebuild();
    expect(await screen.findByText("Contacts refreshed (25)")).toBeInTheDocument();
    await waitFor(() => expect(callsOf("contact_search")).toHaveLength(2));
    await waitFor(() => expect(status()).toHaveTextContent("work: 25 contacts"));

    await user.keyboard("r");
    await waitFor(() => expect(mock.rebuilds).toHaveLength(1));
    settleRebuild({ saved: "refused_shrunk" });
    expect(await screen.findByText("Contacts rebuild found only 3, kept 25 cached")).toBeInTheDocument();

    await user.keyboard("r");
    await waitFor(() => expect(mock.rebuilds).toHaveLength(1));
    settleRebuild({ saved: "refused_empty" });
    expect(await screen.findByText("Contacts rebuild found none, kept 25 cached")).toBeInTheDocument();

    await user.keyboard("r");
    await waitFor(() => expect(mock.rebuilds).toHaveLength(1));
    settleRebuild({ fail: "the store is locked" });
    expect(await screen.findByText("Contacts refresh failed: the store is locked")).toBeInTheDocument();
    // Neither a refusal nor a failure reads the list again.
    expect(callsOf("contact_search")).toHaveLength(2);
  });
});

describe("cancelling a rebuild", () => {
  it("the activity area's card cancels it through operation_cancel, and the end says the daemon may still finish it", async () => {
    const { user } = await openContacts();
    await user.keyboard("r");
    const activity = screen.getByRole("region", { name: "Activity" });
    const card = await within(activity).findByRole("group", { name: "Rebuilding the contact index of work…" });
    await user.click(within(card).getByRole("button", { name: "Cancel the contact rebuild" }));
    await waitFor(() => expect(callsOf("operation_cancel")).toEqual([{ operation_id: "fixture-rebuild-1" }]));
    expect(
      await within(activity).findByText("Stopped waiting for the contact index of work; the daemon may still finish the rebuild"),
    ).toBeInTheDocument();
    await waitFor(() => expect(within(activity).queryByRole("group", { name: /Rebuilding/ })).toBeNull());
    expect(status()).not.toHaveTextContent("Rebuilding");
    // Nothing runs any more, so a new rebuild may start.
    await user.keyboard("r");
    await waitFor(() => expect(callsOf("contact_rebuild")).toHaveLength(2));
  });

  it("a cancel the daemon refuses says so and leaves the card", async () => {
    const { user } = await openContacts();
    await user.keyboard("r");
    const activity = screen.getByRole("region", { name: "Activity" });
    const card = await within(activity).findByRole("group", { name: "Rebuilding the contact index of work…" });
    mock.failing.set("operation_cancel", { kind: "timeout", message: "the daemon did not answer in 5 s" });
    await user.click(within(card).getByRole("button", { name: "Cancel the contact rebuild" }));
    expect(await screen.findByText("The cancel failed: the daemon did not answer in 5 s")).toBeInTheDocument();
    await waitFor(() => expect(within(card).getByRole("button", { name: "Cancel the contact rebuild" })).toBeEnabled());
  });
});

describe("the Contacts view's empty states", () => {
  it("an empty index says to press r", async () => {
    await showContacts(() => {
      mock.contacts.work = [];
    });
    expect(await screen.findByText("No contacts yet; press r to build the index")).toBeInTheDocument();
    expect(screen.queryByRole("listbox", { name: "Contacts" })).toBeNull();
  });
});
