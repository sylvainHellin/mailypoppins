import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitEnvelope, mock, signaturePath, simulateSignatureChanged, SIGNATURE_EDIT_LINE } from "@/test/tauri-mock";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);

async function openSignatures() {
  const { user } = renderApp();
  await shellReady();
  await user.keyboard("cs");
  const dialog = await screen.findByRole("dialog", { name: "Signatures" });
  const list = within(dialog).getByRole("listbox", { name: "Signatures" });
  await within(list).findByRole("option", { name: /short/ });
  return { user, dialog, list };
}

const selected = (list: HTMLElement) => within(list).getByRole("option", { selected: true }).getAttribute("data-signature");
const defaults = (list: HTMLElement) =>
  [...list.querySelectorAll('[data-slot="signature-default"]')].map((b) => b.closest('[role="option"]')?.getAttribute("data-signature"));
const preview = (dialog: HTMLElement) => dialog.querySelector('[data-slot="signature-preview"]') as HTMLElement;

describe("the Signatures dialog", () => {
  it("cs lists every signature with the account's default marked and previews the selected one", async () => {
    const { dialog, list } = await openSignatures();
    expect(callsOf("signature_list")).toEqual([{ account: "work" }]);
    expect(within(list).getAllByRole("option").map((o) => o.getAttribute("data-signature"))).toEqual(["short", "work"]);
    expect(defaults(list)).toEqual(["work"]);
    expect(selected(list)).toBe("short");
    await waitFor(() => expect(preview(dialog)).toHaveTextContent("Me"));
    for (const name of [/New/, /Rename/, /Edit/, /Delete/, /Set default/]) {
      expect(within(dialog).getByRole("button", { name })).toBeEnabled();
    }
  });

  it("j and k move the cursor and the preview follows it", async () => {
    const { user, dialog, list } = await openSignatures();
    await user.keyboard("j");
    expect(selected(list)).toBe("work");
    await waitFor(() => expect(preview(dialog)).toHaveTextContent("Kind regards, Me Fixture GmbH"));
    expect(within(dialog).getByRole("button", { name: /Clear default/ })).toBeInTheDocument();
    await user.keyboard("j");
    expect(selected(list)).toBe("work");
    await user.keyboard("{ArrowUp}");
    expect(selected(list)).toBe("short");
  });

  it("Enter makes the selected signature the default, and clears it when it already is", async () => {
    const { user, list } = await openSignatures();
    await user.keyboard("{Enter}");
    await waitFor(() => expect(defaults(list)).toEqual(["short"]));
    expect(callsOf("signature_set_default")).toEqual([{ account: "work", name: "short" }]);
    expect(await screen.findByText("'short' is now the default signature")).toBeInTheDocument();
    await user.keyboard("{Enter}");
    await waitFor(() => expect(defaults(list)).toEqual([]));
    expect(callsOf("signature_set_default")[1]).toEqual({ account: "work", name: null });
    expect(await screen.findByText("'short' is no longer the default signature")).toBeInTheDocument();
    // Each change reads the listing again: the dialog's own changes publish nothing.
    expect(callsOf("signature_list").length).toBe(3);
  });

  it("e opens the selected signature's file in the editor", async () => {
    const { user } = await openSignatures();
    await user.keyboard("j");
    await user.keyboard("e");
    await waitFor(() => expect(mock.editorOpens).toEqual([signaturePath("work")]));
    expect(await screen.findByText(/Editing signature 'work' in code/)).toBeInTheDocument();
  });

  it("n asks for a name, Enter creates the signature and opens it in the editor", async () => {
    const { user, dialog, list } = await openSignatures();
    await user.keyboard("n");
    const field = within(dialog).getByLabelText("New signature");
    await waitFor(() => expect(field).toHaveFocus());
    // Typed into the field, n, e and d are letters of the name.
    await user.keyboard("newsletter{Enter}");
    expect(callsOf("signature_create")).toEqual([{ name: "newsletter" }]);
    await waitFor(() => expect(mock.editorOpens).toEqual([signaturePath("newsletter")]));
    await waitFor(() => expect(selected(list)).toBe("newsletter"));
    expect(within(dialog).queryByLabelText("New signature")).toBeNull();
    expect(callsOf("signature_delete")).toEqual([]);
  });

  it("a refused name stays in the field with mp_core's sentence, and Escape returns to the list", async () => {
    const { user, dialog, list } = await openSignatures();
    await user.keyboard("n");
    const field = within(dialog).getByLabelText("New signature");
    await waitFor(() => expect(field).toHaveFocus());
    await user.keyboard("work{Enter}");
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("Cannot create: a signature named 'work' already exists");
    expect(field).toHaveValue("work");
    await user.clear(field);
    await user.keyboard("../evil{Enter}");
    await waitFor(() => expect(within(dialog).getByRole("alert")).toHaveTextContent("Cannot create: signature name '../evil' cannot start with a dot"));
    await user.keyboard("{Escape}");
    expect(within(dialog).queryByLabelText("New signature")).toBeNull();
    expect(screen.getByRole("dialog", { name: "Signatures" })).toBeInTheDocument();
    await waitFor(() => expect(list).toHaveFocus());
    await user.keyboard("j");
    expect(selected(list)).toBe("work");
    expect(mock.editorOpens).toEqual([]);
  });

  it("r renames from a field seeded with the name, and the default follows", async () => {
    const { user, dialog, list } = await openSignatures();
    await user.keyboard("j");
    await user.keyboard("r");
    const field = within(dialog).getByLabelText("Rename 'work' to");
    expect(field).toHaveValue("work");
    await waitFor(() => expect(field).toHaveFocus());
    await user.clear(field);
    await user.keyboard("office{Enter}");
    expect(callsOf("signature_rename")).toEqual([{ account: "work", old: "work", new: "office" }]);
    await waitFor(() => expect(defaults(list)).toEqual(["office"]));
    expect(selected(list)).toBe("office");
    expect(await screen.findByText("Renamed 'work' to 'office'")).toBeInTheDocument();
  });

  it("d asks first with the file's path; n keeps it, y deletes it", async () => {
    const { user, list } = await openSignatures();
    await user.keyboard("j");
    await user.keyboard("d");
    const confirm = await screen.findByRole("dialog", { name: "Delete signature 'work'?" });
    expect(confirm).toHaveTextContent(signaturePath("work"));
    await user.keyboard("n");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Delete signature 'work'?" })).toBeNull());
    expect(callsOf("signature_delete")).toEqual([]);
    expect(screen.getByRole("dialog", { name: "Signatures" })).toBeInTheDocument();
    await user.keyboard("d");
    await screen.findByRole("dialog", { name: "Delete signature 'work'?" });
    await user.keyboard("y");
    expect(callsOf("signature_delete")).toEqual([{ account: "work", name: "work" }]);
    await waitFor(() => expect(within(list).getAllByRole("option").map((o) => o.getAttribute("data-signature"))).toEqual(["short"]));
    expect(defaults(list)).toEqual([]);
    expect(selected(list)).toBe("short");
    expect(await screen.findByText("Deleted signature 'work'")).toBeInTheDocument();
    expect(screen.getByRole("dialog", { name: "Signatures" })).toBeInTheDocument();
  });

  it("signature.changed reads the listing and the preview again while it is open", async () => {
    const { user, dialog } = await openSignatures();
    await user.keyboard("j");
    await waitFor(() => expect(preview(dialog)).toHaveTextContent("Fixture GmbH"));
    const reads = callsOf("signature_list").length;
    act(() => simulateSignatureChanged());
    await waitFor(() => expect(callsOf("signature_list").length).toBe(reads + 1));
    await waitFor(() => expect(preview(dialog)).toHaveTextContent(SIGNATURE_EDIT_LINE));
  });

  it("Escape and q close it, and the focus stays inside while it is open", async () => {
    const { user, dialog } = await openSignatures();
    for (let i = 0; i < 10; i++) await user.tab();
    expect(dialog.contains(document.activeElement)).toBe(true);
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Signatures" })).toBeNull());
    await user.keyboard("cs");
    await screen.findByRole("dialog", { name: "Signatures" });
    await user.keyboard("q");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Signatures" })).toBeNull());
    // Reopened, it read the listing again: a delete elsewhere publishes nothing.
    expect(callsOf("signature_list").length).toBe(2);
  });

  it("the palette's Manage signatures opens it; in Contacts c copies and cs opens nothing", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    const palette = await screen.findByRole("dialog", { name: "Command palette" });
    const row = within(palette).getAllByText("Manage signatures")[0].closest("[data-testid='palette-item']");
    expect(row).not.toHaveAttribute("data-disabled", "true");
    await user.keyboard("Manage signatures{Enter}");
    await screen.findByRole("dialog", { name: "Signatures" });
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Signatures" })).toBeNull());
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    await user.keyboard(" c");
    await screen.findByRole("listbox", { name: "Contacts" });
    await user.keyboard("cs");
    expect(writeText).toHaveBeenCalled();
    expect(screen.queryByRole("dialog", { name: "Signatures" })).toBeNull();
  });
});

describe("the new-draft wizard's signature select", () => {
  async function openWizard() {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("cn");
    const dialog = await screen.findByRole("dialog", { name: "New draft" });
    const select = (await within(dialog).findByLabelText("Signature")) as HTMLSelectElement;
    return { user, dialog, select };
  }
  const options = (select: HTMLSelectElement) => [...select.options].map((o) => o.textContent);

  it("follows a signature.changed while the wizard is open: new names, and the default", async () => {
    const { select } = await openWizard();
    expect(select.value).toBe("work");
    mock.signatures.signatures.fresh = "";
    mock.signatures.defaults.work = "short";
    act(() => emitEnvelope("signature.changed", { name: "fresh", path: signaturePath("fresh") }));
    await waitFor(() => expect(options(select)).toEqual(["fresh", "short (default)", "work", "none"]));
    // The options render first, and the effect that follows the listing moves the value one render later.
    await waitFor(() => expect(select.value).toBe("short"));
  });

  it("keeps a pick that is still listed, and falls back to the default when it goes", async () => {
    const { user, select } = await openWizard();
    await user.selectOptions(select, "short");
    mock.signatures.signatures.fresh = "";
    act(() => emitEnvelope("signature.changed", { name: "fresh", path: signaturePath("fresh") }));
    await waitFor(() => expect(options(select)).toContain("fresh"));
    expect(select.value).toBe("short");
    delete mock.signatures.signatures.short;
    act(() => emitEnvelope("signature.changed", { name: "fresh", path: signaturePath("fresh") }));
    await waitFor(() => expect(options(select)).toEqual(["fresh", "work (default)", "none"]));
    await waitFor(() => expect(select.value).toBe("work"));
  });
});
