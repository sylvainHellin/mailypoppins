// PERSO-334: the messages the sync has given up on, in the desktop app. The
// list comes from the bootstrap's `sync_health.given_up` and is replaced by
// every `sync.completed` that ran; the sidebar names each message by mailbox
// and UID, in the server's words, with when mp asks again.
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { reducer, type Action } from "@/app/reducer";
import { givenUpOf, givenUpSentence, givenUpHeadline } from "@/app/givenUp";
import { syncLine } from "@/app/activity";
import { GivenUpState } from "@/components/sidebar/GivenUpState";
import type { AppState } from "@/app/state";
import type { GivenUpMessage, SyncCompleted } from "@/protocol/types";
import { fixtures } from "@/test/tauri-mock";
import { flatState } from "@/test/prefs";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);

const refused: GivenUpMessage = {
  mailbox: "inbox",
  uid: 3,
  kind: "refused",
  reason: "the server answered NO: Some messages could not be FETCHed (Failure)",
  retry_at: "2026-11-21T10:00:00+01:00",
};

const payload = (over: Partial<SyncCompleted>): SyncCompleted => ({
  account: "work",
  severity: "ok",
  saved: 0,
  skipped: 0,
  flags_updated: 0,
  pruned: 0,
  prunes_deferred: 0,
  uid_rebound: 0,
  uidvalidity_resets: 0,
  bodies_truncated: 0,
  non_converging: [],
  failed_mutations: 0,
  error: null,
  ...over,
});

const tick = (over: Partial<SyncCompleted>, revision: number): Action => ({
  type: "gui_event",
  event: {
    type: "event",
    event: { instance_id: "fixture-instance-1", revision, kind: "sync.completed", payload: payload(over) },
  },
});

function booted(): AppState {
  return run(flatState(), {
    type: "gui_event",
    event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap },
  });
}

describe("messages given up on", () => {
  it("are read from the bootstrap, replaced by a tick that ran and kept by one that failed", () => {
    let s = booted();
    expect(givenUpOf(s, "work")).toEqual([]);
    s = run(s, tick({ given_up: [refused] }, 101));
    expect(givenUpOf(s, "work")).toEqual([refused]);
    s = run(s, tick({ severity: "error", error: "login refused" }, 102));
    expect(givenUpOf(s, "work")).toEqual([refused]);
    s = run(s, tick({}, 103));
    expect(givenUpOf(s, "work")).toEqual([]);

    const bootstrap = structuredClone(fixtures.bootstrap);
    const work = bootstrap.snapshot.accounts.find((a) => a.name === "work");
    if (work) work.sync_health.given_up = [refused];
    s = run(flatState(), { type: "gui_event", event: { type: "rebootstrapped", cause: "subscribed", bootstrap } });
    expect(givenUpOf(s, "work")).toEqual([refused]);
  });

  it("read as which message, why in the server's words, and when mp asks again", () => {
    const now = new Date("2026-11-20T09:00:00+01:00");
    const retry = new Date(refused.retry_at as string);
    const at = `${retry.getFullYear()}-${String(retry.getMonth() + 1).padStart(2, "0")}-${String(retry.getDate()).padStart(2, "0")} ${String(retry.getHours()).padStart(2, "0")}:${String(retry.getMinutes()).padStart(2, "0")}`;
    expect(givenUpSentence(refused, now)).toBe(
      `inbox, UID 3 (mp knows it only by its mailbox and number): the server refuses to send it (the server answered NO: Some messages could not be FETCHed (Failure)); mp asks again after ${at}`,
    );
    expect(givenUpSentence(refused, new Date("2026-11-22T00:00:00+01:00"))).toMatch(/; mp asks again on the next sync$/);
    expect(givenUpSentence({ ...refused, kind: "unwritable", retry_at: null }, now)).toMatch(/: mp could not store it \(.*\); mp asks again on every sync$/);
    expect(givenUpHeadline(1)).toBe("1 message could not be downloaded; it is still on the server, nothing is lost");
    expect(givenUpHeadline(2)).toBe("2 messages could not be downloaded; they are still on the server, nothing is lost");
  });

  it("show in the sidebar under their account, and nothing when there are none", () => {
    const { container, rerender } = render(<ul><GivenUpState account="work" givenUp={[refused]} /></ul>);
    expect(screen.getByText("1 message not downloaded")).toBeTruthy();
    expect(container.textContent).toContain("inbox, UID 3");
    expect(container.textContent).toContain("Some messages could not be FETCHed (Failure)");
    expect(container.textContent).toContain("still on the server, nothing is lost");
    rerender(<ul><GivenUpState account="work" givenUp={[]} /></ul>);
    expect(container.textContent).toBe("");
  });

  it("are counted in the activity log's line for a tick", () => {
    expect(syncLine(payload({ given_up: [refused] }))).toMatch(
      /; 1 message could not be downloaded; it is still on the server, nothing is lost \(see the sidebar\)$/,
    );
  });
});
