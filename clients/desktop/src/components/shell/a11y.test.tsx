import { act, render, screen, waitFor, within } from "@testing-library/react";
import type { Dispatch } from "react";
import { windowFor, WINDOW_FROM } from "@/components/list/useWindow";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitEnvelope, fixtures, mock } from "@/test/tauri-mock";
import { ActivityStack } from "@/components/mutations/ActivityStack";
import type { Action } from "@/app/reducer";
import { initialState } from "@/app/state";
import { StoreProvider, useDispatch } from "@/app/store";

describe("accessibility primitives", () => {
  it("has the nav, main and complementary landmarks and a labelled listbox", async () => {
    renderApp();
    await shellReady();
    expect(screen.getByRole("navigation", { name: "Accounts and mailboxes" })).toBeInTheDocument();
    expect(screen.getByRole("main")).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "Reader" })).toBeInTheDocument();
    const list = screen.getByRole("listbox", { name: "Inbox messages" });
    const options = screen.getAllByRole("option");
    expect(options.length).toBe(8);
    for (const o of options) expect(o).toHaveAttribute("aria-selected");
    expect(list).toBeInTheDocument();
  });

  it("gives every row its position and the set size, messages and drafts", async () => {
    const { user } = renderApp();
    await shellReady();
    const options = screen.getAllByRole("option");
    expect(options.map((o) => o.getAttribute("aria-posinset"))).toEqual(options.map((_, i) => String(i + 1)));
    for (const o of options) expect(o).toHaveAttribute("aria-setsize", String(options.length));

    await user.keyboard("2");
    const drafts = await screen.findByRole("listbox", { name: "Drafts messages" });
    const rows = within(drafts).getAllByRole("option");
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.map((o) => o.getAttribute("aria-posinset"))).toEqual(rows.map((_, i) => String(i + 1)));
    for (const o of rows) expect(o).toHaveAttribute("aria-setsize", String(rows.length));
  });

  it("mounts every row: windowing is off in M1", () => {
    expect(WINDOW_FROM).toBe(Number.POSITIVE_INFINITY);
    expect(windowFor(5000, 40_000, 800)).toEqual({ start: 0, end: 5000, padTop: 0, padBottom: 0 });
  });

  it("keeps one tab stop per list (roving tabindex)", async () => {
    const { user } = renderApp();
    await shellReady();
    const stops = () => screen.getAllByRole("option").filter((o) => o.getAttribute("tabindex") === "0");
    expect(stops()).toHaveLength(1);
    await user.keyboard("jj");
    expect(stops()).toHaveLength(1);
    expect(stops()[0]).toHaveAttribute("aria-selected", "true");
    const nav = screen.getByRole("navigation", { name: "Accounts and mailboxes" });
    expect(nav.querySelectorAll('[data-mailbox][tabindex="0"]')).toHaveLength(1);
  });

  it("Tab from the page continues the pane cycle from the focused pane", async () => {
    const { user } = renderApp();
    await shellReady();
    (document.activeElement as HTMLElement | null)?.blur();
    await user.tab();
    // The first Tab from nowhere lands in the pane cycle: sidebar, list, reader.
    const order: (string | null | undefined)[] = [];
    for (let i = 0; i < 3; i++) {
      order.push(document.activeElement?.closest("[data-pane]")?.getAttribute("data-pane"));
      await user.tab();
    }
    expect(order).toEqual(["reader", "sidebar", "list"]);
  });

  it("marks the selected mailbox as the current page", async () => {
    renderApp();
    await shellReady();
    expect(screen.getByRole("button", { name: /^Inbox, 4 unread of 8/ })).toHaveAttribute("aria-current", "page");
  });

  it("makes the views sidebar buttons, one of them or a mailbox the current page, and names each view's region", async () => {
    const { user } = renderApp();
    await shellReady();
    const nav = screen.getByRole("navigation", { name: "Accounts and mailboxes" });
    const current = () => [...nav.querySelectorAll('[aria-current="page"]')].map((e) => e.getAttribute("aria-label"));
    const views = within(nav).getByRole("list", { name: "Views" });
    for (const name of [/^Contacts, key Space c$/, /^Calendar, key Space a$/, /^Settings$/]) {
      const entry = within(views).getByRole("button", { name });
      expect(entry).toBeEnabled();
      expect(entry).toHaveAttribute("tabindex", "-1");
    }
    // Activity is no view: it opens the activity log dialog, and is never the current page.
    const activity = within(views).getByRole("button", { name: "Activity log, key s l" });
    expect(activity).toBeEnabled();
    expect(activity).toHaveAttribute("tabindex", "-1");
    expect(activity).not.toHaveAttribute("aria-current");
    expect(current()).toEqual([expect.stringMatching(/^Inbox, /)]);
    for (const [entry, region] of [
      [/^Contacts, key/, "Contacts"],
      [/^Calendar, key/, "Calendar"],
      [/^Settings$/, "Settings"],
    ] as const) {
      await user.click(within(views).getByRole("button", { name: entry }));
      const pane = await screen.findByRole("region", { name: region });
      expect(within(pane).getByRole("heading", { name: region })).toBeInTheDocument();
      expect(pane.closest("main")).not.toBeNull();
      expect(current()).toEqual([expect.stringMatching(entry)]);
    }
    await user.click(screen.getByRole("button", { name: "Mail" }));
    expect(await screen.findByRole("listbox", { name: "Inbox messages" })).toBeInTheDocument();
    expect(current()).toEqual([expect.stringMatching(/^Inbox, /)]);
  });

  it("names the row controls, keeps them out of the tab order, and says which rows are marked", async () => {
    const { user } = renderApp();
    await shellReady();
    expect(screen.getByRole("listbox", { name: "Inbox messages" })).toHaveAttribute("aria-multiselectable", "true");
    for (const o of screen.getAllByRole("option")) {
      const mark = within(o).getByRole("checkbox", { name: "Mark" });
      const unread = within(o).getByRole("button", { name: "Unread" });
      const flag = within(o).getByRole("button", { name: "Flagged" });
      expect(mark).toHaveAttribute("aria-checked", "false");
      expect(unread).toHaveAttribute("aria-pressed");
      expect(flag).toHaveAttribute("aria-pressed");
      for (const c of [mark, unread, flag]) expect(c).toHaveAttribute("tabindex", "-1");
    }
    await user.keyboard("j");
    const cursor = screen.getAllByRole("option")[0];
    expect(cursor).toHaveAttribute("aria-selected", "true");
    // With marks, aria-selected names the marks and the cursor is the focus.
    await user.keyboard("jv");
    const options = screen.getAllByRole("option");
    expect(options.filter((o) => o.getAttribute("aria-selected") === "true")).toEqual([options[1]]);
    expect(options[1]).toHaveAccessibleName(/marked/);
    expect(options.filter((o) => o.getAttribute("tabindex") === "0")).toEqual([options[2]]);
  });

  it("gives the reader toolbar, the confirmation and the activity area their roles and names", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    const reader = screen.getByRole("complementary", { name: "Reader" });
    const toolbar = await within(reader).findByRole("toolbar", { name: "Message actions" });
    for (const name of ["Archive", "Delete", "Move", "Flag", "Mark unread"]) {
      expect(within(toolbar).getByRole("button", { name })).toBeInTheDocument();
    }

    const activity = screen.getByRole("region", { name: "Activity" });
    const hold = within(activity).getByRole("group", { name: /^Held send: / });
    expect(within(hold).getByRole("progressbar", { name: "Time left before the send" })).toHaveAttribute(
      "aria-valuetext",
      "60 seconds left",
    );
    expect(within(hold).getByRole("button", { name: "Cancel send" })).toBeEnabled();

    await user.keyboard("d");
    const dialog = await screen.findByRole("dialog", { name: "Delete this email?" });
    await waitFor(() => expect(within(dialog).getByRole("button", { name: "Delete" })).toHaveFocus());
  });

  it("mounts the notice and marked-count live regions empty, before their first text", async () => {
    const { user } = renderApp();
    await shellReady();
    const activity = screen.getByRole("region", { name: "Activity" });
    // The seeded hold's card has a status region of its own.
    const notices = activity.querySelector<HTMLElement>('[data-slot="activity-status"]')!;
    expect(notices).toHaveAttribute("role", "status");
    expect(notices).toBeEmptyDOMElement();
    const list = screen.getByRole("region", { name: "Message list" });
    const count = list.querySelector<HTMLElement>('[data-slot="marked-count"]')!;
    expect(count).toHaveAttribute("role", "status");
    expect(count).toBeEmptyDOMElement();

    // The same nodes take the text, so a screen reader hears the first one.
    await user.keyboard("jv");
    expect(list.querySelector('[data-slot="marked-count"]')).toBe(count);
    expect(count).toHaveTextContent("1 marked");
    await user.keyboard("k*");
    await waitFor(() => expect(notices).toHaveTextContent("Flagged"));
    expect(activity.querySelector('[data-slot="activity-status"]')).toBe(notices);
    // A failure stays an alert of its own, outside the polite region.
    mock.failing.set("sync_trigger", { kind: "internal", message: "no route" });
    await user.keyboard("ss");
    const alert = await within(activity).findByRole("alert");
    expect(notices).not.toContainElement(alert);
  });

  it("mounts a hold card's end line empty, and the same node takes Sent", async () => {
    renderApp();
    await shellReady();
    const hold = await screen.findByRole("group", { name: /^Held send:/ });
    const end = within(hold).getByRole("status");
    expect(end).toBeEmptyDOMElement();
    expect(hold).toHaveTextContent("Sending in 60 s");
    act(() => emitEnvelope("send.hold_fired", { ...fixtures.bootstrap.snapshot.holds[0], remaining_secs: 0 }));
    expect(within(hold).getByRole("status")).toBe(end);
    expect(end).toHaveTextContent("Sent");
  });

  it("names the new-draft wizard, starts it in To, and keeps Tab inside it", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("cn");
    const wizard = await screen.findByRole("dialog", { name: "New draft" });
    expect(wizard).toHaveAccessibleDescription(/write it in your editor/);
    await waitFor(() => expect(within(wizard).getByRole("combobox", { name: "To" })).toHaveFocus());
    // Base UI traps focus with a guard on each side and makes the rest of
    // the window inert. Tab onto a guard moves focus back into the dialog on
    // the next animation frame, so each Tab waits for focus to settle inside
    // the wizard before the next one: sampling right after the Tab raced that
    // frame and missed To under a loaded suite. On the way, focus may pass a
    // guard or the body, and no control outside the dialog ever takes it.
    const to = within(wizard).getByRole("combobox", { name: "To" });
    const escaped: Element[] = [];
    const watch = (e: FocusEvent) => {
      const el = e.target as HTMLElement;
      if (!wizard.contains(el) && !el.hasAttribute("data-base-ui-focus-guard") && el !== document.body) escaped.push(el);
    };
    document.addEventListener("focusin", watch);
    const settled = () => waitFor(() => expect(wizard).toContainElement(document.activeElement as HTMLElement));
    const visited: Element[] = [];
    // Cycle until To comes round again; the bound only stops a broken trap
    // from looping forever, the wizard has eight controls.
    for (let i = 0; i < 40; i++) {
      await user.tab();
      await settled();
      visited.push(document.activeElement!);
      if (document.activeElement === to) break;
    }
    expect(visited[visited.length - 1]).toBe(to);
    for (const name of ["Cc", "Bcc"]) expect(visited).toContain(within(wizard).getByRole("combobox", { name }));
    expect(visited).toContain(within(wizard).getByRole("textbox", { name: "Subject" }));
    expect(visited).toContain(within(wizard).getByRole("combobox", { name: "Signature" }));
    expect(visited).toContain(within(wizard).getByRole("button", { name: "Create and edit" }));
    // Shift+Tab from To wraps backwards to the last control, still inside.
    await user.tab({ shift: true });
    await settled();
    expect(document.activeElement).toBe(visited[visited.length - 2]);
    document.removeEventListener("focusin", watch);
    expect(escaped).toEqual([]);
    // The recipient fields complete contacts, so they are comboboxes.
    for (const name of ["To", "Cc", "Bcc"]) expect(within(wizard).getByRole("combobox", { name })).toBeInTheDocument();
    expect(within(wizard).getByRole("textbox", { name: "Subject" })).toBeInTheDocument();
    expect(within(wizard).getByRole("combobox", { name: "Signature" })).toBeInTheDocument();
  });

  it("names the forward and the recipients dialogs", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jj");
    await user.keyboard("cf");
    expect(await screen.findByRole("dialog", { name: "Forward" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    await user.keyboard("2");
    await screen.findByRole("listbox", { name: "Drafts messages" });
    await user.keyboard("j");
    await user.keyboard("ce");
    const recipients = await screen.findByRole("dialog", { name: "Edit recipients" });
    await waitFor(() => expect(within(recipients).getByRole("combobox", { name: "To" })).toHaveFocus());
  });

  it("mounts the editing banner's live region empty, and names its buttons after the draft", async () => {
    const { user } = renderApp();
    await shellReady();
    const region = screen.getByRole("status", { name: "Drafts in the editor" });
    expect(region).toBeEmptyDOMElement();
    await user.keyboard("jr");
    await waitFor(() => expect(region).toHaveTextContent("Editing fixture-draft-1.md"));
    expect(screen.getByRole("status", { name: "Drafts in the editor" })).toBe(region);
    expect(within(region).getByRole("button", { name: "Reopen in editor: fixture-draft-1.md" })).toBeInTheDocument();
    expect(within(region).getByRole("button", { name: "Done editing fixture-draft-1.md" })).toBeInTheDocument();
  });

  it("keeps the activity area and its status region mounted with no hold and no notice", () => {
    let dispatch: Dispatch<Action> = () => {};
    function Grab() {
      dispatch = useDispatch();
      return null;
    }
    render(
      <StoreProvider initial={initialState()}>
        <Grab />
        <ActivityStack />
      </StoreProvider>,
    );
    const area = screen.getByRole("region", { name: "Activity" });
    const status = within(area).getByRole("status");
    expect(status).toBeEmptyDOMElement();
    const done = [{ account: "work", row_id: 1 }];
    act(() => dispatch({ type: "mutation_settled", batch: 1, kind: "archive", account: "work", done, failed: [] }));
    expect(within(area).getByRole("status")).toBe(status);
    expect(status).toHaveTextContent("Archived 1 message");
  });

  it("names the outbox view, keeps its counts and the queue depth in live regions, and names the confirmation", async () => {
    const { user } = renderApp();
    await shellReady();
    const depth = document.querySelector('[data-slot="queue-depth"]') as HTMLElement;
    expect(depth.closest('[aria-live="polite"]')).not.toBeNull();
    await waitFor(() => expect(depth).toHaveTextContent("1 waiting for the server: 1 in the outbox"));
    await user.click(screen.getByRole("button", { name: /Outbox of home/ }));
    const region = await screen.findByRole("region", { name: "Outbox of home" });
    const counts = within(region).getByRole("status");
    await waitFor(() => expect(counts).toHaveTextContent("1 working, 0 failed, 0 partly delivered"));
    expect(within(region).getByRole("list", { name: "Outbox rows of home" })).toBeInTheDocument();
    const row = within(region).getByRole("listitem", { name: "Row 1, Queued" });
    expect(row).toHaveAttribute("tabindex", "0");
    await user.keyboard("d");
    const dialog = await screen.findByRole("dialog", { name: "Discard row 1?" });
    expect(dialog).toHaveAccessibleDescription("<queued-0@home.fixture.example>");
    await waitFor(() => expect(within(dialog).getByRole("button", { name: "Discard" })).toHaveFocus());
  });

  it("names the attach and save dialogs, starts each in its field, ties the picker note to it and keeps the error in an alert", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("ts");
    const save = await screen.findByRole("dialog", { name: "Save attachment" });
    expect(save).toHaveAccessibleDescription("Quarterly ledger review");
    const dir = within(save).getByRole("textbox", { name: "Directory" });
    await waitFor(() => expect(dir).toHaveFocus());
    expect(dir).toHaveAccessibleDescription(/start.*~.*A native picker arrives with the dialog plugin/);
    const alert = within(save).getByRole("alert");
    expect(alert).toBeEmptyDOMElement();
    expect(within(save).getByRole("button", { name: "Save" })).toBeInTheDocument();
    expect(within(save).getByRole("button", { name: "Cancel" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    await user.keyboard("2");
    await screen.findByRole("listbox", { name: "Drafts messages" });
    await user.keyboard("j");
    await user.keyboard("ta");
    const attach = await screen.findByRole("dialog", { name: "Attach file" });
    expect(attach).toHaveAccessibleDescription(/^To the draft /);
    const file = within(attach).getByRole("textbox", { name: "File" });
    await waitFor(() => expect(file).toHaveFocus());
    expect(file).toHaveAccessibleDescription(/A native picker arrives with the dialog plugin/);
    expect(within(attach).getByRole("alert")).toBeEmptyDOMElement();
    await user.keyboard("relative.pdf{Enter}");
    await waitFor(() => expect(within(attach).getByRole("alert")).toHaveTextContent("is not an absolute path"));
    expect(file).toHaveAttribute("aria-invalid", "true");
  });

  it("names each attachment's buttons after the file, in the reader", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    const reader = screen.getByRole("complementary", { name: "Reader" });
    const list = await within(reader).findByRole("list", { name: "Attachments" });
    expect(within(list).getByRole("button", { name: "Open ledger-q3.pdf" })).toHaveAttribute("title", "Open (t o)");
    expect(within(list).getByRole("button", { name: "Save ledger-q3.pdf" })).toHaveAttribute("title", "Save (t s)");
  });
});
