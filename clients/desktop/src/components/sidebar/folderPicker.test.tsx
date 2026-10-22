// The folder picker `gf` opens (ticket 0141): every mailbox of the selected
// account by name, including the folders the daemon found on the server
// without a config entry. The fixture's `work` account gains two of them here.

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { mock } from "@/test/tauri-mock";

const picker = () => screen.findByRole("dialog", { name: "Go to mailbox" });
const nav = () => screen.getByRole("navigation", { name: "Accounts and mailboxes" });
const current = () => nav().querySelector('[aria-current="page"]')?.getAttribute("data-mailbox");

/** Two folders only the server names: a synced one and the counts-only trash. */
function discovered() {
  mock.bootstrap.snapshot.mailboxes.work.push(
    { role: "other", slug: "Projects", label: "Projects", server: "Projects", policy: "sync", total: 0, unread: 0, badge: 0 },
    {
      role: "trash",
      slug: "Deleted Items",
      label: "Trash",
      server: "Deleted Items",
      policy: "status-only",
      total: 0,
      unread: 0,
      badge: 0,
    },
  );
}

describe("the folder picker", () => {
  it("gf lists every mailbox of the account and Enter opens the one typed", async () => {
    const { user } = renderApp(1400, discovered);
    await shellReady();
    await user.keyboard("gf");
    const dialog = await picker();
    const entries = within(dialog)
      .getAllByTestId("folder-entry")
      .map((e) => e.dataset.slug);
    expect(entries).toEqual(["inbox", "drafts", "sent", "archive", "Projects", "Deleted Items"]);
    await user.keyboard("proj{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Go to mailbox" })).toBeNull());
    await waitFor(() => expect(current()).toBe("work/Projects"));
  });

  it("matches a folder by its server name as well as its label", async () => {
    const { user } = renderApp(1400, discovered);
    await shellReady();
    await user.keyboard("gf");
    await picker();
    await user.keyboard("deleted{Enter}");
    await waitFor(() => expect(current()).toBe("work/Deleted Items"));
  });

  it("the palette's Go to mailbox opens it", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    await screen.findByRole("dialog", { name: "Command palette" });
    await user.keyboard("Go to mailbox (fuzzy{Enter}");
    expect(await picker()).toBeInTheDocument();
  });
});
