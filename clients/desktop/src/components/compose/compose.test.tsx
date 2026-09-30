import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitEnvelope, mock } from "@/test/tauri-mock";
import { draftName, fwdSubject, normalizeRecipients } from "@/app/compose";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
const banner = () => document.querySelector('[data-slot="editing-banner"]') as HTMLElement;
const reader = () => screen.getByRole("complementary", { name: "Reader" });

type User = ReturnType<typeof renderApp>["user"];

async function drafts(user: User) {
  await user.keyboard("2");
  return screen.findByRole("listbox", { name: "Drafts messages" });
}

async function openWizard(user: User) {
  await user.keyboard("cn");
  const dialog = await screen.findByRole("dialog", { name: "New draft" });
  await waitFor(() => expect(within(dialog).getByLabelText("To")).toHaveFocus());
  await within(dialog).findByLabelText("Signature");
  return dialog;
}

/** A file the listing skipped because it does not parse. */
function brokenDraft() {
  mock.drafts.work.skipped = [{ path: "/fixture/work/drafts/broken.md", error: "line 2: mapping values are not allowed here" }];
}

describe("the compose wizard", () => {
  it("has the TUI's fields, the default signature preselected, and a none option", async () => {
    const { user } = renderApp();
    await shellReady();
    const dialog = await openWizard(user);
    for (const label of ["To", "Cc", "Bcc", "Subject"]) expect(within(dialog).getByLabelText(label)).toHaveValue("");
    const signature = within(dialog).getByLabelText("Signature") as HTMLSelectElement;
    expect(signature.value).toBe("work");
    expect([...signature.options].map((o) => o.textContent)).toEqual(["short", "work (default)", "none"]);
    // No inline body: draft_create takes none, so the body is written in the editor.
    expect(within(dialog).queryByRole("textbox", { name: /body/i })).toBeNull();
  });

  it("Enter moves to the next field, and Cmd+Enter creates the draft and opens it in the editor", async () => {
    const { user } = renderApp();
    await shellReady();
    const dialog = await openWizard(user);
    await user.keyboard("kim@example.com{Enter}");
    expect(within(dialog).getByLabelText("Cc")).toHaveFocus();
    await user.keyboard("{Enter}{Enter}Hello there{Enter}");
    expect(within(dialog).getByLabelText("Signature")).toHaveFocus();
    await user.selectOptions(within(dialog).getByLabelText("Signature"), "short");
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(callsOf("draft_create")).toHaveLength(1));
    const args = callsOf("draft_create")[0] as Record<string, unknown>;
    expect(args).toMatchObject({
      account: "work",
      signature: "short",
      no_signature: null,
      headers: { to: "kim@example.com", cc: "", bcc: "", subject: "Hello there" },
    });
    expect(String(args.name)).toMatch(/^draft-\d{4}-\d{2}-\d{2}-\d{6}-hello-there$/);
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New draft" })).toBeNull());
    await waitFor(() => expect(mock.editorOpens).toEqual([`/fixture/work/drafts/${String(args.name)}.md`]));
    await waitFor(() => expect(banner()).toHaveTextContent(`Editing ${String(args.name)}.md in code --wait`));
  });

  it("none carries no signature", async () => {
    const { user } = renderApp();
    await shellReady();
    const dialog = await openWizard(user);
    await user.keyboard("kim@example.com");
    await user.selectOptions(within(dialog).getByLabelText("Signature"), "none");
    await user.click(within(dialog).getByRole("button", { name: /Create and edit/ }));
    await waitFor(() => expect(callsOf("draft_create")).toHaveLength(1));
    expect(callsOf("draft_create")[0]).toMatchObject({ signature: null, no_signature: true });
  });

  it("refuses a draft with no recipient, as the TUI does, and stays open", async () => {
    const { user } = renderApp();
    await shellReady();
    const dialog = await openWizard(user);
    await user.keyboard("{Control>}{Enter}{/Control}");
    expect(await within(dialog).findByText("Add a recipient in To, Cc or Bcc")).toBeInTheDocument();
    expect(callsOf("draft_create")).toEqual([]);
    expect(screen.getByRole("dialog", { name: "New draft" })).toBeInTheDocument();
  });

  it("shows the daemon's refusal in the dialog, which stays open", async () => {
    const { user } = renderApp();
    await shellReady();
    mock.failing.set("draft_create", {
      kind: "not_found",
      message: "draft.create: the daemon refused the call: A draft already exists at /fixture/work/drafts/x.md (-32602)",
      code: -32602,
    });
    const dialog = await openWizard(user);
    await user.keyboard("kim@example.com{Control>}{Enter}{/Control}");
    expect(await within(dialog).findByText(/A draft already exists/)).toBeInTheDocument();
    expect(mock.editorOpens).toEqual([]);
  });

  it("Escape cancels and writes nothing", async () => {
    const { user } = renderApp();
    await shellReady();
    await openWizard(user);
    await user.keyboard("kim@example.com{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New draft" })).toBeNull());
    expect(callsOf("draft_create")).toEqual([]);
  });
});

describe("the recipients dialog", () => {
  it("rewrites the recipients and keeps the subject when it did not change", async () => {
    const { user } = renderApp();
    await shellReady();
    await drafts(user);
    await user.keyboard("j");
    await user.keyboard("ce");
    const dialog = await screen.findByRole("dialog", { name: "Edit recipients" });
    const to = within(dialog).getByLabelText("To");
    await waitFor(() => expect(to).toHaveFocus());
    await user.clear(to);
    await user.keyboard("kim@example.com, ");
    await user.type(within(dialog).getByLabelText("Bcc"), "boss@example.com");
    await user.click(within(dialog).getByRole("button", { name: "Save recipients" }));
    await waitFor(() =>
      expect(callsOf("draft_set_recipients")).toEqual([
        { account: "work", id: "angebot-antwort", to: "kim@example.com", cc: "", bcc: "boss@example.com", subject: null },
      ]),
    );
    expect(await screen.findByText("Recipients updated: mp://work/drafts/angebot-antwort")).toBeInTheDocument();
    expect(mock.editorOpens).toEqual([]);
    await waitFor(() => expect(within(reader()).getByText("boss@example.com")).toBeInTheDocument());
  });
});

describe("the editing banner", () => {
  it("names the draft and the editor; Reopen runs the editor again and Done ends the session", async () => {
    const { user } = renderApp();
    await shellReady();
    expect(banner()).toBeEmptyDOMElement();
    await user.keyboard("j");
    await user.keyboard("r");
    await waitFor(() => expect(banner()).toHaveTextContent("Editing fixture-draft-1.md in code --wait '/fixture/work/drafts/fixture-draft-1.md'"));
    await user.click(within(banner()).getByRole("button", { name: /Reopen in editor/ }));
    await waitFor(() => expect(mock.editorOpens).toHaveLength(2));
    await user.click(within(banner()).getByRole("button", { name: /Done editing/ }));
    expect(banner()).toBeEmptyDOMElement();
  });

  it("ends when the draft is removed, and shows the editing mark on its row until then", async () => {
    const { user } = renderApp();
    await shellReady();
    await drafts(user);
    await user.keyboard("j");
    await user.keyboard("e");
    const row = () => document.querySelector('[data-draft-id="angebot-antwort"]');
    await waitFor(() => expect(row()?.querySelector('[data-slot="draft-editing"]')).not.toBeNull());
    expect(row()).toHaveAccessibleName(/open in the editor/);
    act(() => emitEnvelope("state.remove", { resource: "draft:work/angebot-antwort" }));
    await waitFor(() => expect(banner()).toBeEmptyDOMElement());
  });

  it("an editor that did not start is a failure notice naming what to set", async () => {
    const { user } = renderApp();
    await shellReady();
    mock.editorFailure = {
      kind: "setup",
      message: "The editor did not start: could not start `nope`: not found; check MP_DESKTOP_EDITOR.",
    };
    await user.keyboard("j");
    await user.keyboard("r");
    const alert = await screen.findByRole("alert", { name: "" });
    await waitFor(() => expect(alert).toHaveTextContent(/The editor did not open fixture-draft-1\.md: .*MP_DESKTOP_EDITOR/));
    expect(banner().querySelector('[data-status="error"]')).toHaveTextContent("fixture-draft-1.md did not open in the editor");
  });
});

describe("approve and demote", () => {
  it("a draft that does not parse is refused, and the alert names why and where", async () => {
    const { user } = renderApp(1400, brokenDraft);
    await shellReady();
    const list = await drafts(user);
    const row = within(list).getByRole("option", { name: /invalid: line 2: mapping values/ });
    expect(row.querySelector('[data-slot="draft-status"]')).toHaveTextContent("invalid");
    expect(row).toHaveAttribute("title", "line 2: mapping values are not allowed here");
    await user.keyboard("G");
    await user.keyboard("cA");
    await waitFor(() => expect(callsOf("draft_approve")).toEqual([{ account: "work", ids: ["broken"] }]));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Could not approve 1 draft; it keeps its status");
    expect(alert).toHaveTextContent("does not parse: line 2: mapping values are not allowed here (/fixture/work/drafts/broken.md)");
    expect(row.querySelector('[data-slot="draft-status"]')).toHaveTextContent("invalid");
  });
});

describe("the draft preview", () => {
  it("shows the headers, the status, the validation and the body of the selected draft", async () => {
    const { user } = renderApp();
    await shellReady();
    await drafts(user);
    await user.keyboard("j");
    const preview = await within(reader()).findByRole("article", { name: "Draft: Re: Angebot Dachsanierung" });
    expect(await within(preview).findByText("robin@example.com")).toBeInTheDocument();
    expect(within(preview).getByText("Valid")).toBeInTheDocument();
    expect(preview.querySelector('[data-slot="draft-status"]')).toHaveTextContent("draft");
    expect(preview.querySelector('[data-slot="draft-body"]')).toHaveTextContent("vielen Dank für das Angebot");
    expect(callsOf("draft_validate")).toContainEqual({ account: "work", id: "angebot-antwort" });

    await user.keyboard("j");
    const other = await within(reader()).findByRole("article", { name: "Draft: (no subject)" });
    expect(await within(other).findByText(/Not sendable: no recipient/)).toBeInTheDocument();
    expect(within(other).getByRole("list", { name: "Warnings" })).toHaveTextContent("the subject is empty");
  });

  it("Enter opens a draft's preview in the reader", async () => {
    const { user } = renderApp();
    await shellReady();
    await drafts(user);
    await user.keyboard("j{Enter}");
    await waitFor(() => expect(document.activeElement?.closest("[data-pane]")?.getAttribute("data-pane")).toBe("reader"));
    expect(await within(reader()).findByRole("article", { name: "Draft: Re: Angebot Dachsanierung" })).toBeInTheDocument();
    expect(callsOf("draft_preview")).toContainEqual({ account: "work", id: "angebot-antwort" });
  });

  it("a draft that does not parse shows why instead of a preview", async () => {
    const { user } = renderApp(1400, brokenDraft);
    await shellReady();
    await drafts(user);
    await user.keyboard("G");
    const preview = await within(reader()).findByRole("article", { name: "Draft: broken" });
    expect(within(preview).getByText("This draft does not parse")).toBeInTheDocument();
    expect(within(preview).getByText("line 2: mapping values are not allowed here")).toBeInTheDocument();
    expect(callsOf("draft_preview")).toEqual([]);
  });
});

describe("the reader toolbar", () => {
  it("Reply, Reply all and Forward act on the open message", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jj{Enter}");
    const toolbar = await within(reader()).findByRole("toolbar", { name: "Message actions" });
    await user.click(within(toolbar).getByRole("button", { name: "Reply all" }));
    await waitFor(() => expect(callsOf("draft_reply")).toEqual([{ account: "work", row_id: 1002, all: true, headers: null }]));
    await user.click(within(toolbar).getByRole("button", { name: "Forward" }));
    expect(await screen.findByRole("dialog", { name: "Forward" })).toBeInTheDocument();
  });
});

describe("compose helpers", () => {
  it("names a new draft after the time and the subject, as the TUI wizard does", () => {
    const at = new Date(2026, 8, 30, 9, 5, 7);
    expect(draftName("Re: Angebot / Dach?", at)).toBe("draft-2026-09-30-090507-re-angebot-dach");
    expect(draftName("", at)).toBe("draft-2026-09-30-090507");
  });

  it("builds the forward subject by mp_core's rule and trims recipient separators", () => {
    expect(fwdSubject("Hello")).toBe("Fwd: Hello");
    expect(fwdSubject("fwd: Hello")).toBe("fwd: Hello");
    expect(normalizeRecipients("a@example.com, b@example.com, ")).toBe("a@example.com, b@example.com");
  });
});

describe("while a draft is being sent", () => {
  const BUSY = "That draft is being sent; it cannot change until the send ends";

  async function sending(user: User) {
    await drafts(user);
    await user.keyboard("jx");
    await screen.findByRole("dialog", { name: "Draft is not approved. Approve and send?" });
    await user.keyboard("y");
    await waitFor(() => expect(document.querySelector('[data-draft-id="angebot-antwort"]')).toHaveAttribute("data-sending", "true"));
  }

  it.each(["d", "cA", "cD", "e", "ce", "x"])("%s on the sending draft is refused with a notice", async (keys) => {
    const { user } = renderApp();
    await shellReady();
    await sending(user);
    expect(screen.queryByText(BUSY)).toBeNull();
    await user.keyboard(keys);
    expect(await screen.findByText(BUSY)).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(callsOf("draft_discard")).toEqual([]);
    expect(callsOf("draft_approve")).toEqual([]);
    expect(callsOf("draft_demote")).toEqual([]);
    expect(callsOf("draft_path")).toEqual([]);
    expect(mock.editorOpens).toEqual([]);
    expect(callsOf("send_draft")).toHaveLength(1);
  });

  it("the other draft is not held up, and the settle frees the sent one", async () => {
    const { user } = renderApp();
    await shellReady();
    await sending(user);
    await user.keyboard("j");
    await user.keyboard("cA");
    await waitFor(() => expect(callsOf("draft_approve")).toEqual([{ account: "work", ids: ["offsite-note"] }]));
    act(() => emitEnvelope("operation.finished", { operation_id: "fixture-send-1", state: "cancelled", error: { code: -32008, message: "operation_cancelled" } }));
    await user.keyboard("k");
    await user.keyboard("e");
    await waitFor(() => expect(mock.editorOpens).toEqual(["/fixture/work/drafts/angebot-antwort.md"]));
  });
});
