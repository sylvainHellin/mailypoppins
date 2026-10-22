import { beforeEach, describe, expect, it, vi } from "vitest";
import { messageItems, openHtml } from "@/app/attachments";
import type { Action } from "@/app/reducer";
import { initialState, type AppState } from "@/app/state";
import { fixtures, mock, resetMock } from "@/test/tauri-mock";
import type { DraftListing } from "@/protocol/types";

const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

/** Drafts of `work` shown, the cursor on `draft`. */
function onDraft(draft: string, listing: DraftListing = clone(fixtures.drafts.work)): AppState {
  const s = initialState();
  return {
    ...s,
    messages: { ...s.messages, data: { kind: "drafts", account: "work", listing } },
    selection: { ...s.selection, account: "work", mailbox: "drafts", draft },
  };
}

const calls = (name: string) => mock.calls.filter((c) => c.cmd === name).map((c) => c.args);

describe("tb on a draft (PERSO-136)", () => {
  beforeEach(() => resetMock());

  it("opens the HTML the send would build, through draft_html_open", async () => {
    const dispatch = vi.fn<(a: Action) => void>();
    await openHtml(onDraft("angebot-antwort"), dispatch);
    expect(calls("draft_html_open")).toEqual([{ account: "work", id: "angebot-antwort" }]);
    expect(mock.opened).toEqual(["/fixture/cache/renditions/draft-angebot-antwort/message.html"]);
    expect(calls("html_open")).toEqual([]);
    expect(dispatch).toHaveBeenCalledWith({ type: "notice", text: "Opened in browser" });
    expect(dispatch).not.toHaveBeenCalledWith(expect.objectContaining({ text: "No HTML version available" }));
  });

  it("a refused draft.html says why on the activity line and opens nothing", async () => {
    const dispatch = vi.fn<(a: Action) => void>();
    mock.failing.set("draft_html_open", { kind: "not_found", message: "no draft matches mp://work/drafts/angebot-antwort", code: -32602 });
    await openHtml(onDraft("angebot-antwort"), dispatch);
    expect(mock.opened).toEqual([]);
    expect(dispatch).toHaveBeenCalledWith({
      type: "activity",
      kind: "failed",
      account: "work",
      text: "Draft preview failed: no draft matches mp://work/drafts/angebot-antwort",
    });
  });

  it("a draft that does not parse asks for the editor and calls nothing", async () => {
    const listing = clone(fixtures.drafts.work);
    listing.skipped = [{ path: "/fixture/work/drafts/broken.md", error: "line 2: mapping values are not allowed here" }];
    const dispatch = vi.fn<(a: Action) => void>();
    await openHtml(onDraft("broken", listing), dispatch);
    expect(calls("draft_html_open")).toEqual([]);
    expect(dispatch).toHaveBeenCalledWith({ type: "notice", text: "This draft does not parse; fix it in the editor first" });
  });
});

describe("a message's inline images (PERSO-165)", () => {
  it("follow the real attachments, tagged, each keeping the daemon's part index", () => {
    const items = messageItems([
      { name: "image001.png", size: 12, inline: true },
      { name: "Rechnung.pdf", size: 8, inline: false },
    ]);
    expect(items).toEqual([
      { part: 1, name: "Rechnung.pdf", size: 8, missing: false, inline: false },
      { part: 0, name: "image001.png", size: 12, missing: false, inline: true },
    ]);
  });
});
