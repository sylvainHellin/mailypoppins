import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { mock } from "@/test/tauri-mock";
import { acceptRecipient, recipientQuery } from "@/app/compose";
import { NO_CONTACTS_HINT, RECIPIENT_DEBOUNCE_MS } from "@/components/compose/RecipientsInput";

type User = ReturnType<typeof renderApp>["user"];

const searches = () => mock.calls.filter((c) => c.cmd === "contact_search").map((c) => c.args as Record<string, unknown>);
const lastSearch = () => searches()[searches().length - 1];

async function openWizard(user: User) {
  await user.keyboard("cn");
  const dialog = await screen.findByRole("dialog", { name: "New draft" });
  await waitFor(() => expect(within(dialog).getByRole("combobox", { name: "To" })).toHaveFocus());
  await within(dialog).findByLabelText("Signature");
  return dialog;
}

const field = (dialog: HTMLElement, name: string) => within(dialog).getByRole("combobox", { name }) as HTMLInputElement;
const list = (dialog: HTMLElement) => within(dialog).findByRole("listbox", { name: "Contacts" });
const options = (box: HTMLElement) => within(box).getAllByRole("option");
/** Longer than the debounce and an answer, so a list that would open has opened. */
const settle = () => act(() => new Promise((r) => setTimeout(r, RECIPIENT_DEBOUNCE_MS * 3)));

describe("recipient completion helpers", () => {
  it("completes the text after the last comma, trimmed", () => {
    expect(recipientQuery("")).toBe("");
    expect(recipientQuery("ro")).toBe("ro");
    expect(recipientQuery("kim@example.com,  ro ")).toBe("ro");
    expect(recipientQuery("kim@example.com, ")).toBe("");
  });

  it("formats as the TUI's accept_suggestion does", () => {
    expect(acceptRecipient("ro", { display_name: "Robin Meyer", address: "robin@example.com" })).toBe("Robin Meyer <robin@example.com>, ");
    expect(acceptRecipient("a@b.c,ja", { display_name: "Doe, Jane", address: "jane@x.y" })).toBe('a@b.c, "Doe, Jane" <jane@x.y>, ');
    expect(acceptRecipient("a@b.c, op", { display_name: "", address: "ops@example.com" })).toBe("a@b.c, ops@example.com, ");
  });
});

describe("recipient completion in the compose wizard", () => {
  it("lists the contacts matching the text after the last comma, the first highlighted, and Enter rewrites only that text", async () => {
    const { user } = renderApp();
    await shellReady();
    const dialog = await openWizard(user);
    const to = field(dialog, "To");
    expect(to).toHaveAttribute("aria-expanded", "false");
    await user.keyboard("kim@example.com, ro");
    const box = await list(dialog);
    expect(lastSearch()).toEqual({ account: "work", query: "ro", limit: 12 });
    const rows = options(box);
    expect(rows.map((r) => r.textContent)).toEqual([
      "Robin Meyerrobin@example.com",
      "Marco Rossimarco.rossi@supplier.example",
      "Olivia Brownolivia@example.com",
    ]);
    expect(rows[0]).toHaveAttribute("aria-selected", "true");
    expect(to).toHaveAttribute("aria-expanded", "true");
    expect(to).toHaveAttribute("aria-controls", box.id);
    await waitFor(() => expect(to).toHaveAttribute("aria-activedescendant", rows[0].id));
    await user.keyboard("{Enter}");
    expect(to).toHaveValue("kim@example.com, Robin Meyer <robin@example.com>, ");
    expect(to).toHaveFocus();
    expect(within(dialog).queryByRole("listbox", { name: "Contacts" })).toBeNull();
    expect(to).toHaveAttribute("aria-expanded", "false");
  });

  it("quotes a name that holds a comma", async () => {
    const { user } = renderApp();
    await shellReady();
    const dialog = await openWizard(user);
    await user.keyboard("jane");
    await list(dialog);
    await user.keyboard("{Enter}");
    expect(field(dialog, "To")).toHaveValue('"Doe, Jane" <jane.doe@example.com>, ');
  });

  it("Tab accepts and keeps the focus, a bare address with no name", async () => {
    const { user } = renderApp();
    await shellReady();
    const dialog = await openWizard(user);
    await user.keyboard("{Enter}");
    const cc = field(dialog, "Cc");
    expect(cc).toHaveFocus();
    await user.keyboard("ops@");
    await list(dialog);
    await user.keyboard("{Tab}");
    expect(cc).toHaveValue("ops@example.com, ");
    expect(cc).toHaveFocus();
  });

  it("ArrowDown then Enter accepts the second row", async () => {
    const { user } = renderApp();
    await shellReady();
    const dialog = await openWizard(user);
    await user.keyboard("ro");
    const box = await list(dialog);
    await user.keyboard("{ArrowDown}");
    await waitFor(() => expect(options(box)[1]).toHaveAttribute("aria-selected", "true"));
    expect(field(dialog, "To")).toHaveAttribute("aria-activedescendant", options(box)[1].id);
    await user.keyboard("{Enter}");
    expect(field(dialog, "To")).toHaveValue("Marco Rossi <marco.rossi@supplier.example>, ");
  });

  it("a click accepts", async () => {
    const { user } = renderApp();
    await shellReady();
    const dialog = await openWizard(user);
    await user.keyboard("ro");
    const box = await list(dialog);
    await user.click(within(box).getByRole("option", { name: /Marco Rossi/ }));
    expect(field(dialog, "To")).toHaveValue("Marco Rossi <marco.rossi@supplier.example>, ");
    expect(field(dialog, "To")).toHaveFocus();
  });

  it("Escape closes the list and keeps the text and the dialog; the list stays closed until the text changes", async () => {
    const { user } = renderApp();
    await shellReady();
    const dialog = await openWizard(user);
    await user.keyboard("ro");
    await list(dialog);
    await user.keyboard("{Escape}");
    expect(within(dialog).queryByRole("listbox", { name: "Contacts" })).toBeNull();
    expect(screen.getByRole("dialog", { name: "New draft" })).toBeInTheDocument();
    const to = field(dialog, "To");
    expect(to).toHaveValue("ro");
    await settle();
    expect(within(dialog).queryByRole("listbox", { name: "Contacts" })).toBeNull();
    await user.keyboard("b");
    expect(await list(dialog)).toBeInTheDocument();
    expect(to).toHaveValue("rob");
  });

  it("Enter with no list moves to the next field", async () => {
    const { user } = renderApp();
    await shellReady();
    const dialog = await openWizard(user);
    await user.keyboard("nobody@nowhere.example");
    await settle();
    expect(within(dialog).queryByRole("listbox", { name: "Contacts" })).toBeNull();
    await user.keyboard("{Enter}");
    expect(field(dialog, "Cc")).toHaveFocus();
    expect(field(dialog, "To")).toHaveValue("nobody@nowhere.example");
  });

  it("ignores an answer that a newer query overtook", async () => {
    const { user } = renderApp();
    await shellReady();
    const dialog = await openWizard(user);
    let release!: () => void;
    mock.gates.set("contact_search", new Promise<void>((r) => (release = r)));
    await user.keyboard("k");
    await waitFor(() => expect(lastSearch()).toMatchObject({ query: "k" }));
    await user.keyboard("{Backspace}s");
    const box = await list(dialog);
    await waitFor(() => expect(options(box)[0]).toHaveTextContent("Sam Okafor"));
    await act(async () => release());
    await settle();
    expect(options(await list(dialog))[0]).toHaveTextContent("Sam Okafor");
    expect(field(dialog, "To")).toHaveValue("s");
  });

  it("says once per focus that there are no contacts yet when the index is empty", async () => {
    const { user } = renderApp();
    await shellReady();
    mock.contacts.work = [];
    const dialog = await openWizard(user);
    await user.keyboard("ro");
    const box = await list(dialog);
    expect(options(box).map((o) => o.textContent)).toEqual([NO_CONTACTS_HINT]);
    expect(options(box)[0]).toHaveAttribute("aria-disabled", "true");
    // The hint takes no key: Escape closes it, and it does not come back in this focus.
    await user.keyboard("{Escape}");
    expect(within(dialog).queryByRole("listbox", { name: "Contacts" })).toBeNull();
    await user.keyboard("b");
    await settle();
    expect(within(dialog).queryByRole("listbox", { name: "Contacts" })).toBeNull();
    // Enter moves on as with no list; a new focus may say it again.
    await user.keyboard("{Enter}");
    expect(field(dialog, "Cc")).toHaveFocus();
    await user.keyboard("x");
    expect(options(await list(dialog)).map((o) => o.textContent)).toEqual([NO_CONTACTS_HINT]);
    await user.keyboard("{Enter}");
    expect(field(dialog, "Bcc")).toHaveFocus();
  });

  it("says nothing when the index has contacts but none matches", async () => {
    const { user } = renderApp();
    await shellReady();
    const dialog = await openWizard(user);
    await user.keyboard("qqq");
    await settle();
    expect(within(dialog).queryByRole("listbox", { name: "Contacts" })).toBeNull();
    await waitFor(() => expect(searches()).toContainEqual({ account: "work", query: "", limit: 1 }));
  });

  it("leaves the subject a plain field", async () => {
    const { user } = renderApp();
    await shellReady();
    const dialog = await openWizard(user);
    await user.keyboard("{Enter}{Enter}{Enter}");
    const subject = within(dialog).getByRole("textbox", { name: "Subject" });
    expect(subject).toHaveFocus();
    await user.keyboard("ro");
    await settle();
    expect(within(dialog).queryByRole("listbox")).toBeNull();
    expect(searches()).toEqual([]);
  });
});
