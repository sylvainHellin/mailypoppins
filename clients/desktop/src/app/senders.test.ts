import { describe, expect, it } from "vitest";
import { blockChoice, blockedSendersLoaded, blockSummary } from "@/app/senders";
import { reducer, type Action } from "@/app/reducer";
import { initialState, isStale, type AppState } from "@/app/state";
import { comparableAddress } from "@/lib/mailbox";
import { fixtures } from "@/test/tauri-mock";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);

const rebootstrapped: Action = { type: "gui_event", event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap } };

function changed(entries: string[], revision: number): Action {
  return {
    type: "gui_event",
    event: { type: "event", event: { instance_id: fixtures.bootstrap.instance_id, revision, kind: "sender.blocklist_changed", payload: { entries } } },
  };
}

describe("the blocked senders model", () => {
  it("offers the address a reply goes to, as the list holds it, and its domain", () => {
    expect(blockChoice("Offers <Offers@Shop.Example>")).toEqual({ address: "offers@shop.example", domain: "@shop.example" });
    expect(blockChoice("x@y.org, Alice <alice@z.org>")).toEqual({ address: "alice@z.org", domain: "@z.org" });
    expect(blockChoice('John <"John.Doe"@Example.com>')).toEqual({ address: "john.doe@example.com", domain: "@example.com" });
    for (const none of ["(unknown)", "", "Undisclosed recipients:;", "@example.com"]) expect(blockChoice(none)).toBeNull();
  });

  it("compares an address as mp_core does: lowercase, a quoted local part unquoted where it needs no quotes", () => {
    expect(comparableAddress("Spam@Example.COM")).toBe("spam@example.com");
    expect(comparableAddress('"John.Doe"@Example.com')).toBe("john.doe@example.com");
    expect(comparableAddress('"john doe"@example.com')).toBe('"john doe"@example.com');
    expect(comparableAddress('"john..doe"@example.com')).toBe('"john..doe"@example.com');
    expect(comparableAddress("(unknown)")).toBe("(unknown)");
  });

  it("says what a block came to in the TUI's words", () => {
    const skipped = [{ account: "home", reason: "home has no Junk folder; its Inbox keeps the sender's mail" }];
    expect(blockSummary({ entry: "@x.example", added: true, entries: ["@x.example"], moved: [{ account: "work", mailbox: "junk", count: 1 }], skipped: [] })).toBe(
      "Blocked @x.example; 1 message moved to Junk (work 1)",
    );
    expect(
      blockSummary({
        entry: "a@x.example",
        added: false,
        entries: ["a@x.example"],
        moved: [
          { account: "work", mailbox: "junk", count: 2 },
          { account: "home", mailbox: "Junk", count: 3 },
        ],
        skipped,
      }),
    ).toBe(`a@x.example was already blocked; 5 messages moved to Junk (work 2, home 3); ${skipped[0].reason}`);
    expect(blockSummary({ entry: "a@x.example", added: true, entries: [], moved: [], skipped: [] })).toBe("Blocked a@x.example; no Inbox mail from it");
  });

  it("drops a list read that an event overtook, and reads again after a bootstrap", () => {
    let s = run(initialState(), rebootstrapped, { type: "open_blocked_senders" });
    expect(s.overlay).toBe("blocked_senders");
    expect(isStale(s.blockedSenders)).toBe(true);
    const asked = s.blockedSenders.gen;
    s = run(s, changed(["new@x.example"], 900));
    expect(s.blockedSenders.data).toEqual(["new@x.example"]);
    expect(isStale(s.blockedSenders)).toBe(false);
    expect(blockedSendersLoaded(s, asked, ["old@x.example"]).blockedSenders.data).toEqual(["new@x.example"]);
    s = run(s, { type: "blocked_senders_set", entries: [] });
    expect(s.blockedSenders.data).toEqual([]);
    expect(isStale(run(s, rebootstrapped).blockedSenders)).toBe(true);
  });
});
