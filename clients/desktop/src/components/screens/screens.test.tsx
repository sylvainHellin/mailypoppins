import { act, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emit, fixtures, mock } from "@/test/tauri-mock";
import type { ConnectError } from "@/lib/gui-types";

const unavailable: ConnectError = {
  kind: "unavailable",
  why: "nothing listens on the socket and the on-demand start timed out",
  socket: "/run/user/501/mailypoppins/daemon.sock",
  log: "/Users/me/.local/share/mailypoppins/logs/daemon.log",
  daemon_version: null,
  daemon_protocol: null,
};

describe("the connection screens", () => {
  it("shows skeletons while connecting", async () => {
    renderApp(1400, () => {
      mock.connection = { state: "connecting" };
    });
    expect(await screen.findByText(/Connecting to the mailypoppins daemon/)).toBeInTheDocument();
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("shows why the daemon is unavailable, where to look, Retry and a confirmed restart", async () => {
    const { user } = renderApp(1400, () => {
      mock.connection = { state: "failed", error: unavailable };
    });
    expect(await screen.findByRole("heading", { name: /daemon is not available/ })).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent(unavailable.why);
    expect(screen.getByText(unavailable.socket)).toBeInTheDocument();
    expect(screen.getByText(unavailable.log)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Retry/ }));
    expect(mock.calls.some((c) => c.cmd === "retry_connect")).toBe(true);

    await user.click(screen.getByRole("button", { name: /Restart daemon/ }));
    const dialog = await screen.findByRole("dialog");
    expect(mock.calls.some((c) => c.cmd === "restart_daemon")).toBe(false);
    await user.click(within(dialog).getByRole("button", { name: "Restart daemon" }));
    expect(mock.calls.some((c) => c.cmd === "restart_daemon")).toBe(true);
  });

  it("blocks on a version mismatch with the app, protocol and daemon versions", async () => {
    renderApp(1400, () => {
      mock.connection = {
        state: "failed",
        error: { ...unavailable, kind: "version_mismatch", why: "the daemon speaks protocol 2", daemon_version: "9.9.9" },
      };
    });
    expect(await screen.findByRole("heading", { name: /incompatible version/ })).toBeInTheDocument();
    expect(await screen.findByText("0.1.0")).toBeInTheDocument();
    expect(screen.getByText("1 to 1")).toBeInTheDocument();
    expect(screen.getByText("9.9.9")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Restart daemon/ })).toBeInTheDocument();
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("names the daemon's version and protocol from a refused handshake, and what to do (PERSO-277)", async () => {
    const why =
      "the running daemon is mailypoppins 0.11.0 (protocol 1) and lacks calendar.reminders, which this client (mailypoppins 0.12.0, protocol 1) requires; restart the daemon to run the matching version";
    renderApp(1400, () => {
      mock.connection = {
        state: "failed",
        error: { ...unavailable, kind: "version_mismatch", why, daemon_version: "0.11.0", daemon_protocol: { min: 1, max: 2 } },
      };
    });
    expect(await screen.findByRole("heading", { name: /incompatible version/ })).toBeInTheDocument();
    expect(screen.getByText("0.11.0")).toBeInTheDocument();
    expect(screen.getByText("Daemon protocol")).toBeInTheDocument();
    expect(screen.getByText("1 to 2")).toBeInTheDocument();
    expect(screen.getByText(/Restart the daemon to run the version this app expects/)).toBeInTheDocument();
  });

  it("says a daemon that does not report its version is older, never unknown (PERSO-277)", async () => {
    renderApp(1400, () => {
      mock.connection = {
        state: "failed",
        error: { ...unavailable, kind: "version_mismatch", why: "the daemon refused the handshake" },
      };
    });
    expect(await screen.findByRole("heading", { name: /incompatible version/ })).toBeInTheDocument();
    expect(screen.getByText(/not reported: an older daemon/)).toBeInTheDocument();
    expect(screen.queryByText("unknown")).toBeNull();
    expect(screen.queryByText("Daemon protocol")).toBeNull();
  });

  it("turns a reconnect to a daemon of another version into the restart screen, and back", async () => {
    renderApp();
    await shellReady();
    act(() => emit({ type: "disconnected", reason: "the socket closed" }));
    const why = "the running daemon is mailypoppins 0.9.0, but this app starts /Applications/mailypoppins.app/Contents/MacOS/mp (mailypoppins 0.10.0); restart the daemon to run the matching version";
    act(() =>
      emit({
        type: "connection",
        status: { state: "failed", error: { ...unavailable, kind: "version_mismatch", why, daemon_version: "0.9.0" } },
      }),
    );
    expect(await screen.findByRole("heading", { name: /incompatible version/ })).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent(why);
    expect(screen.getByText("0.9.0")).toBeInTheDocument();
    expect(screen.queryByRole("listbox")).toBeNull();
    act(() => emit({ type: "reconnected", instance_id: "fixture-instance-2" }));
    act(() =>
      emit({
        type: "connection",
        status: { state: "connected", instance_id: "fixture-instance-2", daemon_version: "0.10.0", protocol: 1, fixture: false },
      }),
    );
    act(() => emit({ type: "rebootstrapped", cause: "reconnected", bootstrap: fixtures.bootstrap }));
    expect(await screen.findAllByRole("listbox")).not.toHaveLength(0);
  });

  it("shows a reconnecting banner, then a resync banner that the next bootstrap clears", async () => {
    renderApp();
    await shellReady();
    act(() => emit({ type: "disconnected", reason: "the socket closed" }));
    expect(await screen.findByText(/Reconnecting to the daemon: the socket closed/)).toBeInTheDocument();
    act(() => emit({ type: "reconnected", instance_id: "fixture-instance-1" }));
    act(() => emit({ type: "resync", instance_id: "fixture-instance-1", reason: "event_queue_overflow" }));
    expect(await screen.findByText(/Resynchronising with the daemon \(event_queue_overflow\)/)).toBeInTheDocument();
    act(() => emit({ type: "rebootstrapped", cause: "resync", bootstrap: fixtures.bootstrap }));
    expect(screen.queryByText(/Resynchronising/)).toBeNull();
    // The banners live in a polite live region.
    expect(document.querySelector('[aria-live="polite"]')).not.toBeNull();
  });
});
