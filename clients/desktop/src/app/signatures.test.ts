import { describe, expect, it } from "vitest";
import { reducer, type Action } from "@/app/reducer";
import { cursorAfterRemoval, keptCursor, signaturesWanted } from "@/app/signatures";
import { initialState, isStale, type AppState } from "@/app/state";
import { fixtures } from "@/test/tauri-mock";
import type { SignatureListing } from "@/lib/gui-types";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);

function booted(): AppState {
  return run(initialState(), {
    type: "gui_event",
    event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap },
  });
}

function envelope(kind: string, payload: unknown, revision = 900): Action {
  return { type: "gui_event", event: { type: "event", event: { instance_id: fixtures.bootstrap.instance_id, revision, kind, payload } } };
}

const listing = (account: string, names = ["short", "work"], dflt: string | null = "work"): SignatureListing => ({ account, names, default: dflt });

/** The dialog open on `work`, its listing read. */
function withDialog(): AppState {
  const s = run(booted(), { type: "open_signatures", account: "work" });
  return run(s, { type: "signatures_loaded", account: "work", gen: s.signatures.work.gen, listing: listing("work") });
}

describe("the signature listings", () => {
  it("the dialog opens on the account and asks for its listing, again on every open", () => {
    const s = run(booted(), { type: "open_signatures", account: "work" });
    expect(s.overlay).toBe("signatures");
    expect(s.signaturesDialog).toEqual({ account: "work" });
    expect(isStale(s.signatures.work)).toBe(true);
    expect(signaturesWanted(s)).toBe("work");
    const loaded = withDialog();
    expect(isStale(loaded.signatures.work)).toBe(false);
    const closed = run(loaded, { type: "overlay", overlay: null });
    expect(closed.signaturesDialog).toBeNull();
    expect(signaturesWanted(closed)).toBeNull();
    // A delete elsewhere publishes nothing: the next open reads it again.
    const reopened = run(closed, { type: "open_signatures", account: "work" });
    expect(isStale(reopened.signatures.work)).toBe(true);
    expect(reopened.signatures.work.data).toEqual(listing("work"));
  });

  it("a signature.changed during a read leaves the listing stale when that read lands", () => {
    let s = run(booted(), { type: "open_signatures", account: "work" });
    const asked = s.signatures.work.gen;
    s = run(s, envelope("signature.changed", { name: "new" }));
    s = run(s, { type: "signatures_loaded", account: "work", gen: asked, listing: listing("work") });
    expect(isStale(s.signatures.work)).toBe(true);
  });

  it("the new-draft wizard asks for its account's listing; forward and recipients do not", () => {
    const s = run(booted(), { type: "open_compose", dialog: { kind: "new", account: "home" } });
    expect(signaturesWanted(s)).toBe("home");
    expect(isStale(s.signatures.home)).toBe(true);
    const fwd = run(booted(), { type: "open_compose", dialog: { kind: "forward", account: "work", row_id: 1001, subject: "Fwd: x" } });
    expect(signaturesWanted(fwd)).toBeNull();
    expect(fwd.signatures).toEqual({});
  });

  it("signature.changed, the dialog's own change and a bootstrap make every listing stale", () => {
    let s = withDialog();
    s = run(s, { type: "open_compose", dialog: { kind: "new", account: "home" } });
    s = run(s, { type: "signatures_loaded", account: "home", gen: s.signatures.home.gen, listing: listing("home", ["short", "work"], null) });
    expect(isStale(s.signatures.work) || isStale(s.signatures.home)).toBe(false);
    const changed = run(s, envelope("signature.changed", { name: "work", path: "/c/signatures/work.md" }));
    expect(isStale(changed.signatures.work) && isStale(changed.signatures.home)).toBe(true);
    // Only the open wizard's is read.
    expect(signaturesWanted(changed)).toBe("home");
    const own = run(s, { type: "signatures_changed" });
    expect(isStale(own.signatures.work) && isStale(own.signatures.home)).toBe(true);
    const again = run(s, { type: "gui_event", event: { type: "rebootstrapped", cause: "resync", bootstrap: fixtures.bootstrap } });
    expect(isStale(again.signatures.work) && isStale(again.signatures.home)).toBe(true);
  });

  it("a failed read is kept as the listing's error, and the next change reads it again", () => {
    const s = run(booted(), { type: "open_signatures", account: "work" });
    const asked = s.signatures.work.gen;
    const failed = run(s, { type: "signatures_failed", account: "work", gen: asked, error: { kind: "internal", message: "boom" } });
    expect(failed.signatures.work.error?.message).toBe("boom");
    expect(isStale(failed.signatures.work)).toBe(false);
    const changed = run(failed, envelope("signature.changed", { name: "work", path: "/c/signatures/work.md" }));
    expect(isStale(changed.signatures.work)).toBe(true);
  });

  it("another dialog closes it, and a removed account closes it", () => {
    const s = withDialog();
    const other = run(s, { type: "open_compose", dialog: { kind: "new", account: "work" } });
    expect(other.signaturesDialog).toBeNull();
    const removed = run(s, envelope("state.remove", { resource: "account:work" }));
    expect(removed.overlay).toBeNull();
    expect(removed.signaturesDialog).toBeNull();
    expect(removed.signatures.work).toBeUndefined();
  });

  it("the cursor keeps its name, else the first row; a removed row hands it to the next, else the one before", () => {
    expect(keptCursor(["a", "b"], "b")).toBe("b");
    expect(keptCursor(["a", "b"], "gone")).toBe("a");
    expect(keptCursor([], null)).toBeNull();
    expect(cursorAfterRemoval(["a", "b", "c"], "b")).toBe("c");
    expect(cursorAfterRemoval(["a", "b", "c"], "c")).toBe("b");
    expect(cursorAfterRemoval(["a"], "a")).toBeNull();
  });
});
