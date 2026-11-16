// The fixture backend's `note_*` and `agent_*` commands (PERSO-315), which
// answer as src-tauri/src/fixture/notes.rs and the daemon do.

import { beforeEach, describe, expect, it } from "vitest";
import * as cmd from "@/lib/commands";
import { closeNote, mock, resetMock } from "@/test/tauri-mock";
import type { GuiEvent } from "@/lib/gui-types";

const INBOX_FIRST = "mp://work/inbox/quarterly-ledger-review-1001@fixture.example";

/** Every `note.changed` the backend emitted, by its payload's ids. */
function noteEvents(): string[][] {
  return events.flatMap((e) => (e.type === "event" && e.event.kind === "note.changed" ? [(e.event.payload as { notes: { id: string }[] }).notes.map((n) => n.id)] : []));
}

let events: GuiEvent[] = [];

beforeEach(() => {
  resetMock();
  events = [];
  mock.channel = { onmessage: (e: GuiEvent) => events.push(e) } as never;
});

describe("the fixture's notes", () => {
  it("answer the daemon's presets, and a set on a message marks its row and every listing", async () => {
    expect((await cmd.notePresets()).presets.map((p) => p.key)).toEqual(["r", "d", "s", "e", "p", "t", "f"]);
    const set = await cmd.noteSet({ selector: INBOX_FIRST, preset: "s" });
    expect(set).toMatchObject({ account: "work", selector: INBOX_FIRST, message_id: "quarterly-ledger-review-1001@fixture.example", state: "open", subject: "Quarterly ledger review" });
    expect(noteEvents()).toEqual([[set.id]]);
    const flat = await cmd.listMessages("work", "inbox");
    expect(flat.kind === "messages" && flat.rows.find((r) => r.id === 1001)?.note).toBe("open");
    const grouped = await cmd.listMessages("work", "inbox", true);
    expect(grouped.kind === "conversations" && grouped.rows.find((r) => r.latest.id === 1001)?.note).toBe("open");
    expect((await cmd.noteGet(INBOX_FIRST)).note?.id).toBe(set.id);
  });

  it("set_many puts one note on each mail, a draft's on its draft row, and refuses with the daemon's sentences", async () => {
    const many = await cmd.noteSetMany({ selectors: [INBOX_FIRST, "mp://work/drafts/angebot-antwort"], text: "Answer by Friday" });
    expect(many.notes.map((n) => n.selector)).toEqual([INBOX_FIRST, "mp://work/drafts/angebot-antwort"]);
    const drafts = await cmd.listMessages("work", "drafts");
    expect(drafts.kind === "drafts" && drafts.listing.drafts.find((d) => d.id === "angebot-antwort")?.note).toBe("open");
    await expect(cmd.noteSet({ selector: INBOX_FIRST })).rejects.toEqual({ kind: "protocol", message: "a note needs a preset or a text", code: null });
    await expect(cmd.noteSet({ selector: INBOX_FIRST, preset: "z" })).rejects.toMatchObject({ message: "preset z is not configured; the presets are r, d, s, e, p, t, f" });
    await expect(cmd.noteSet({ text: "hi" })).rejects.toMatchObject({ message: expect.stringMatching(/^a note for a new email names the account/) });
  });

  it("export changes nothing, handoff marks the exported notes, and clear_done drops the done ones", async () => {
    const fresh = await cmd.noteSet({ account: "home", text: "Ask the landlord about the heating" });
    const onMail = await cmd.noteSet({ selector: INBOX_FIRST, preset: "s" });
    const exported = await cmd.noteExport();
    expect(exported.ids).toEqual([onMail.id, fresh.id]);
    expect(exported.text).toMatch(/^I have notes on 2 emails\./);
    expect(exported.text).toContain(`New email from account home (note ${fresh.id})`);
    expect((await cmd.noteList()).notes.every((n) => n.state === "open")).toBe(true);
    const handed = await cmd.noteHandoff(exported.ids);
    expect(handed.text).toBe(exported.text);
    expect((await cmd.noteList()).notes.map((n) => n.state)).toEqual(["handed_off", "handed_off"]);
    expect((await cmd.noteExport()).text).toBe("");
    closeNote(onMail.id, "Three lines posted");
    expect((await cmd.noteClearDone()).removed).toBe(1);
    expect((await cmd.noteClear(fresh.id)).removed).toBe(1);
    await expect(cmd.noteClear(fresh.id)).rejects.toMatchObject({ message: `no note has the id ${fresh.id}` });
  });

  it("the agent's settings are the built-in ones, and paseo and the panel's spawn are recorded", async () => {
    expect(await cmd.agentSettings()).toEqual({ route: "panel", name: "pi", command: ["pi", "--name", "mail notes {date}", "@{batch}", "Work through these notes."], cwd: "/home/fixture/pi-remote" });
    expect(await cmd.agentPaseo("prompt", "2026-11-16")).toBe("fixture");
    expect(mock.paseoRuns).toEqual([{ text: "prompt", date: "2026-11-16" }]);
  });
});
