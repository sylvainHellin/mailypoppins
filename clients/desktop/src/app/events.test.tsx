import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emit, fixtures, mock } from "@/test/tauri-mock";

const listCalls = () => mock.calls.filter((c) => c.cmd === "list_messages").length;

describe("live events end to end", () => {
  it("refetches the shown list when its mailbox is invalidated", async () => {
    renderApp();
    await shellReady();
    const before = listCalls();
    act(() =>
      emit({
        type: "event",
        event: { instance_id: "fixture-instance-1", revision: 101, kind: "state.invalidate", payload: { resource: "mailbox:work/inbox", scope: { query: "counts" } } },
      }),
    );
    await waitFor(() => expect(listCalls()).toBeGreaterThan(before));
  });

  it("keeps the open message across a daemon restart that renumbered the rows", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jj");
    const reader = screen.getByRole("complementary", { name: "Reader" });
    expect(await within(reader).findByRole("heading", { name: "Angebot Dachsanierung" })).toBeInTheDocument();

    mock.rowShift = 500;
    act(() => emit({ type: "disconnected", reason: "fixture: simulated daemon restart" }));
    act(() => emit({ type: "reconnected", instance_id: "fixture-instance-2" }));
    act(() =>
      emit({ type: "rebootstrapped", cause: "instance_changed", bootstrap: { ...fixtures.bootstrap, instance_id: "fixture-instance-2" } }),
    );
    await waitFor(() =>
      expect(document.querySelector('[role="option"][aria-selected="true"]')?.getAttribute("data-row-id")).toBe("1502"),
    );
    expect(await within(reader).findByRole("heading", { name: "Angebot Dachsanierung" })).toBeInTheDocument();
    expect(mock.calls.some((c) => c.cmd === "message_html_meta" && c.args?.row_id === 1502)).toBe(true);
    expect(within(reader).getByTitle(/^Message body/)).toHaveAttribute("src", "mpmsg://localhost/work/1502");
    expect(screen.queryByText(/Resynchronising|Reconnecting/)).toBeNull();
  });
});
