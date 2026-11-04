import { describe, expect, it } from "vitest";
import { GUI_ENTRIES, paletteEntries, type ActionId, type PaletteEntry } from "@/keymap/catalog";
import { paletteScore, rankPalette } from "@/components/palette/rank";

/** The palette's real rows, in the order it lists them. */
const CATALOG: PaletteEntry[] = [...paletteEntries(), ...GUI_ENTRIES];

const labels = (query: string, recent: string[] = []) => rankPalette(CATALOG, query, recent).map((e) => e.label);
const rows = (query: string) => rankPalette(CATALOG, query, []).map((e) => `${e.section} ${e.label}`);

const entry = (label: string, keys: string[] = [], id: ActionId | null = null, section = "APP"): PaletteEntry => ({
  section,
  label,
  keys,
  id,
  badge: null,
});

describe("the palette's ranking", () => {
  it("puts a label match above rows that match by their section only", () => {
    const shown = rankPalette(CATALOG, "cal", []);
    const top = ["Calendar reminders (snooze, dismiss, open)", "Switch to Calendar view", "Show or hide calendars"];
    expect(shown.slice(0, 3).map((e) => e.label)).toEqual(top);
    // Every CALENDAR row still matches, by its section, below them.
    const calendar = CATALOG.filter((e) => e.section === "CALENDAR");
    expect(shown.filter((e) => e.section === "CALENDAR")).toHaveLength(calendar.length);
    expect(shown.slice(3).every((e) => paletteScore(e, "cal") < paletteScore(shown[0], "cal"))).toBe(true);
  });

  it("hides a row whose letters only turn up scattered", () => {
    expect(labels("test")).toEqual(["Show a test reminder notification"]);
    expect(paletteScore(entry("Toggle flag/star"), "test")).toBe(0);
    expect(paletteScore(entry("Dismiss the newest notice"), "test")).toBe(0);
  });

  it("matches a word's start, and three letters or more inside a word", () => {
    expect(labels("not")).toEqual(["Show a test reminder notification", "Dismiss the newest notice", "Dismiss all notices"]);
    expect(labels("box")).toContain("Open outbox");
    // Two letters inside a word are not enough.
    expect(paletteScore(entry("Open outbox"), "bo")).toBe(0);
  });

  it("finds both Archive rows first", () => {
    expect(rows("arch").slice(0, 2)).toEqual(["MESSAGE (list, headers, body) Archive", "SERVER SEARCH Archive"]);
  });

  it("needs every word of the query, each at a word's start", () => {
    expect(labels("new ev")[0]).toBe("New event (the event form, on the cursor's day)");
    expect(labels("new ev")).not.toContain("New draft");
    expect(labels("dismiss not").slice(0, 2)).toEqual(["Dismiss the newest notice", "Dismiss all notices"]);
  });

  it("matches a row by its key, below a label match", () => {
    expect(labels("ff")).toContain("Search all mail (sender, subject, body)");
    expect(labels("x")[0]).toBe("Send current draft (approve + send)");
    // `X` is the case-exact key of the dismiss.
    expect(labels("X")[0]).toBe("Dismiss the newest notice");
    expect(labels(":")).toEqual(["Command palette (run an action by name)"]);
    const go = labels("go");
    expect(go.indexOf("Open outbox")).toBeGreaterThan(go.indexOf("Go to message list"));
    const byKey = entry("Send", ["x"]);
    const byLabel = entry("X marks the spot");
    expect(rankPalette([byKey, byLabel], "x", [])).toEqual([byLabel, byKey]);
  });

  it("breaks a tie by the action run most recently, then by catalog order", () => {
    expect(labels("not", ["dismiss_all_notices"]).slice(0, 2)).toEqual(["Dismiss all notices", "Show a test reminder notification"]);
    expect(labels("not", ["dismiss_notice", "dismiss_all_notices"]).slice(0, 2)).toEqual([
      "Dismiss the newest notice",
      "Dismiss all notices",
    ]);
    // Recency never lifts a weaker match.
    expect(labels("cal", ["calendar_today"]).slice(0, 3)).toEqual([
      "Calendar reminders (snooze, dismiss, open)",
      "Switch to Calendar view",
      "Show or hide calendars",
    ]);
    expect(labels("cal", ["calendar_today"])[3]).toBe("Go to today");
  });

  it("keeps nothing for an empty query", () => {
    expect(rankPalette(CATALOG, "  ", [])).toEqual([]);
  });
});
