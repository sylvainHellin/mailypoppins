import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { DraftItem } from "@/app/state";
import type { DraftSyncState } from "@/protocol/generated";
import { DraftRow, draftSyncLabel } from "@/components/list/DraftRow";

afterEach(cleanup);

function draft(state?: DraftSyncState): DraftItem {
  return {
    id: "d1",
    selector: "mp://work/drafts/d1",
    path: "/d/d1.md",
    status: "draft",
    to: "robin@example.com",
    cc: null,
    bcc: null,
    subject: "Angebot",
    date: null,
    valid: true,
    ready: true,
    server: state === undefined ? undefined : { state, synced_at: null },
    diagnostic: null,
  };
}

function rowOf(item: DraftItem) {
  render(
    <DraftRow
      draft={item}
      editing={false}
      cursor={false}
      marked={false}
      anyMarked={false}
      pending={false}
      tabStop
      position={1}
      setSize={1}
      onSelect={() => {}}
      onMark={() => {}}
    />,
  );
  return screen.getByRole("option");
}

describe("a draft's server state (#0143)", () => {
  it("shows pending, synced, conflict and failed as a badge and in the row's name", () => {
    for (const state of ["pending", "synced", "conflict", "failed"] as const) {
      const row = rowOf(draft(state));
      const badge = row.querySelector('[data-slot="draft-sync"]');
      expect(badge?.getAttribute("data-sync")).toBe(state);
      expect(badge?.textContent).toContain(state);
      expect(row.getAttribute("aria-label")).toContain(`server copy ${state}`);
      cleanup();
    }
  });

  it("shows nothing for a draft that is not mirrored or has no state yet", () => {
    for (const item of [draft("local"), draft()]) {
      const row = rowOf(item);
      expect(row.querySelector('[data-slot="draft-sync"]')).toBeNull();
      expect(row.getAttribute("aria-label")).not.toContain("server copy");
      cleanup();
    }
    expect(draftSyncLabel("local")).toBeNull();
  });
});
