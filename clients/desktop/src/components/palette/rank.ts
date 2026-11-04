// The command palette's search: which rows a query keeps, and their order.
//
// Each query token must match a word of the row: the whole word, its start,
// or, at three letters or more, a run inside a word of the label. Letters
// that only turn up scattered ("test" in "Toggle flag/star") match nothing,
// so the row is hidden. A label match counts twice what the same match in
// the section or the keys counts; the whole query equal to one of the row's
// keys (`ff`, `x`, `Ctrl+p`) is a key match. Equal scores go to the action
// run from the palette most recently, then to catalog order.

import type { PaletteEntry } from "@/keymap/catalog";

const LABEL = 2;
const OTHER = 1;
const WHOLE = 3;
const START = 2;
const INSIDE = 1;
/** A shorter token matches only at the start of a word. */
const INSIDE_MIN = 3;

function words(text: string): string[] {
  return text.toLowerCase().split(/[^\p{L}\p{N}]+/u).filter(Boolean);
}

function wordScore(token: string, of: string[], inside: boolean): number {
  let best = 0;
  for (const w of of) {
    if (w === token) return WHOLE;
    if (w.startsWith(token)) best = START;
    else if (inside && best < INSIDE && token.length >= INSIDE_MIN && w.includes(token)) best = INSIDE;
  }
  return best;
}

/** How well `query` matches the row, 0 for no match; higher is better. */
export function paletteScore(e: PaletteEntry, query: string): number {
  const q = query.trim();
  if (q === "") return 0;
  // A case-exact key (`X`, not `x`) is a whole match, a key in another case a start.
  const key = e.keys.includes(q) ? WHOLE : e.keys.some((k) => k.toLowerCase() === q.toLowerCase()) ? START : 0;
  const tokens = words(q);
  // A query of punctuation alone (`:`, `?`) can only be a key.
  if (tokens.length === 0) return OTHER * key;
  const label = words(e.label);
  const section = words(e.section);
  let total = 0;
  for (const t of tokens) {
    const s = Math.max(LABEL * wordScore(t, label, true), OTHER * wordScore(t, section, false), OTHER * key);
    if (s === 0) return 0;
    total += s;
  }
  return total;
}

/**
 * The rows `query` keeps, best first: by score, then by the place of their
 * action in `recent` (most recent first), then in the order given.
 */
export function rankPalette<T extends PaletteEntry>(entries: readonly T[], query: string, recent: readonly string[]): T[] {
  const age = (e: T) => {
    const i = e.id === null ? -1 : recent.indexOf(e.id);
    return i < 0 ? recent.length : i;
  };
  return entries
    .map((e, index) => ({ e, index, score: paletteScore(e, query), age: age(e) }))
    .filter((r) => r.score > 0)
    .sort((a, b) => b.score - a.score || a.age - b.age || a.index - b.index)
    .map((r) => r.e);
}
