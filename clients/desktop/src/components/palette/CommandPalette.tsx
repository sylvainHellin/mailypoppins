import { useMemo, useState } from "react";
import {
  CommandDialog,
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { BadgeFor } from "@/components/palette/BadgeFor";
import { Keys } from "@/components/palette/Keys";
import { rankPalette } from "@/components/palette/rank";
import { GUI_ENTRIES, paletteEntries, type ActionId, type PaletteEntry } from "@/keymap/catalog";

function groups(entries: PaletteEntry[]): [string, PaletteEntry[]][] {
  const out = new Map<string, PaletteEntry[]>();
  for (const e of entries) out.set(e.section, [...(out.get(e.section) ?? []), e]);
  return [...out];
}

const NO_EXTRA: PaletteEntry[] = [];
const NO_HIDDEN: ReadonlySet<ActionId> = new Set();
const NO_RECENT: readonly string[] = [];

export type CommandPaletteProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onRun: (id: ActionId) => void;
  /** Rows that come and go with the model, such as the update's (`updatePaletteEntries`), after the fixed ones. */
  extra?: PaletteEntry[];
  /** Actions whose rows the model leaves out for now, such as the reader mode's on a draft (`hiddenPaletteRows`). */
  hidden?: ReadonlySet<ActionId>;
  /** The actions last run from the palette, most recent first (`prefs.paletteRecent`). */
  recent?: readonly string[];
};

/**
 * Every KEYMAP action, with its keys; what M1 cannot run yet is listed,
 * disabled, with the milestone that brings it.
 */
export function CommandPalette({
  open,
  onOpenChange,
  onRun,
  extra = NO_EXTRA,
  hidden = NO_HIDDEN,
  recent = NO_RECENT,
}: CommandPaletteProps) {
  const all = useMemo(
    () => groups([...paletteEntries(), ...GUI_ENTRIES, ...extra].filter((e) => e.id === null || !hidden.has(e.id))),
    [extra, hidden],
  );
  const run = (id: ActionId) => {
    onOpenChange(false);
    // Run after the dialog closes, so focus moves where the action puts it.
    setTimeout(() => onRun(id), 0);
  };
  return (
    <CommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Command palette"
      description="Run an action by name"
      className="sm:max-w-xl"
    >
      <PaletteBody all={all} recent={recent} onRun={run} />
    </CommandDialog>
  );
}

/**
 * The palette's input and rows, mounted with the dialog's popup, so each
 * opening starts with an empty search. With no search the rows sit under
 * their sections; while a search runs they are one list, ranked by
 * `rankPalette` (rank.ts) and drawn in that order, cmdk's own filter off: a
 * label match first, the section and the keys below the same match in the
 * label, rows whose letters only turn up scattered hidden, and equal matches
 * by the action last run from the palette, then catalog order. Out of their
 * groups, the rows name their section, so the same label in two sections
 * (Archive, Forward, Jump to top) reads as two actions.
 */
function PaletteBody({
  all,
  recent,
  onRun,
}: {
  all: [string, PaletteEntry[]][];
  recent: readonly string[];
  onRun: (id: ActionId) => void;
}) {
  const [search, setSearch] = useState("");
  const searching = search.trim() !== "";
  const ranked = useMemo(
    () => (searching ? rankPalette(all.flatMap(([, entries]) => entries), search, recent) : []),
    [all, search, searching, recent],
  );
  const row = (section: string, e: PaletteEntry) => (
    <CommandItem
      key={`${section}:${e.label}`}
      value={`${section}:${e.label}`}
      disabled={e.id === null}
      data-testid="palette-item"
      data-label={e.label}
      onSelect={() => {
        if (e.id) onRun(e.id);
      }}
      className="data-[disabled=true]:opacity-100 data-[disabled=true]:text-disabled-foreground"
    >
      <span className="min-w-0 flex-1 truncate">{e.label}</span>
      {searching ? (
        <span
          data-slot="palette-section"
          className="max-w-[40%] truncate text-xs text-muted-foreground group-data-selected/command-item:text-selection-foreground"
        >
          {section}
        </span>
      ) : null}
      {e.badge ? <BadgeFor badge={e.badge} /> : null}
      <Keys keys={e.keys} />
    </CommandItem>
  );
  return (
    <Command loop shouldFilter={false}>
      <CommandInput placeholder="Run an action by name…" aria-label="Action name" value={search} onValueChange={setSearch} />
      <CommandList className="max-h-[60vh]">
        <CommandEmpty>No action matches.</CommandEmpty>
        {!searching
          ? all.map(([section, entries]) => (
              <CommandGroup key={section} heading={section}>
                {entries.map((e) => row(section, e))}
              </CommandGroup>
            ))
          : ranked.map((e) => row(e.section, e))}
      </CommandList>
    </Command>
  );
}
