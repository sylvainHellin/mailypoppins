// Cancelling a running sync from the desktop (PERSO-321): the activity
// area's card for each sync this window started, its Cancel and the TUI's
// `sx`, all through `operation_cancel`, the call the TUI's `sx` and the CLI's
// ^C make.

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { act } from "react";
import { emitEnvelope, mock, settleSync } from "@/test/tauri-mock";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
const activity = () => screen.getByRole("region", { name: "Activity" });
const CANCELLED = "Sync of work cancelled: what it already ingested stays, and the next sync goes on from there";
const NONE = "No sync to cancel: none started from this window is running";
const CUT = "Sync of work cut short by the daemon stopping: what it already ingested stays, and the next sync goes on from there";

describe("cancelling a sync", () => {
  it("sS shows a running card whose Cancel cancels the sync, and the end says what stays", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("sS");
    const card = await within(activity()).findByRole("group", { name: "Full sync of work…" });
    await user.click(within(card).getByRole("button", { name: "Cancel the sync" }));
    await waitFor(() => expect(callsOf("operation_cancel")).toEqual([{ operation_id: "fixture-op-1" }]));
    expect(await within(activity()).findByText(CANCELLED)).toBeInTheDocument();
    await waitFor(() => expect(within(activity()).queryByRole("group", { name: /sync of work/ })).toBeNull());
    // A cancel is not a failed sync.
    expect(screen.queryByText(/Sync of work failed/)).toBeNull();
  });

  it("sx cancels the sync this window started, and with none running says so and calls nothing", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("sx");
    expect(await screen.findByText(NONE)).toBeInTheDocument();
    expect(callsOf("operation_cancel")).toEqual([]);

    await user.keyboard("ss");
    await within(activity()).findByRole("group", { name: "Quick sync of work…" });
    await user.keyboard("sx");
    await waitFor(() => expect(callsOf("operation_cancel")).toEqual([{ operation_id: "fixture-op-1" }]));
    expect(await within(activity()).findByText(CANCELLED)).toBeInTheDocument();
  });

  it("a cancel that came too late says nothing was cancelled, and the sync's own end takes the card away", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("sS");
    const card = await within(activity()).findByRole("group", { name: "Full sync of work…" });
    // The pass ended on its own; its finish is still on the way.
    settleSync({ silent: true });
    await user.click(within(card).getByRole("button", { name: "Cancel the sync" }));
    expect(await screen.findByText("The sync of work had already ended: nothing was cancelled")).toBeInTheDocument();
    expect(screen.queryByText(CANCELLED)).toBeNull();
  });

  it("a sync the daemon's stop cancelled says the stop cut it short", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("sS");
    await within(activity()).findByRole("group", { name: "Full sync of work…" });
    const id = mock.syncs[0].operation_id;
    act(() => emitEnvelope("daemon.shutting_down", { grace_secs: 0, pending: [] }));
    act(() => emitEnvelope("operation.finished", { operation_id: id, state: "cancelled", error: { code: -32008, message: `operation ${id} was cancelled` } }));
    expect(await within(activity()).findByText(CUT)).toBeInTheDocument();
    expect(screen.queryByText(CANCELLED)).toBeNull();
  });

  it("a sync's own end takes its card away", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("sS");
    await within(activity()).findByRole("group", { name: "Full sync of work…" });
    settleSync();
    await waitFor(() => expect(within(activity()).queryByRole("group", { name: /sync of work/ })).toBeNull());
  });
});
