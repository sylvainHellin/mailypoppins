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
import { GUI_ENTRIES, paletteEntries, type ActionId, type PaletteEntry } from "@/keymap/catalog";

function groups(entries: PaletteEntry[]): [string, PaletteEntry[]][] {
  const out = new Map<string, PaletteEntry[]>();
  for (const e of entries) out.set(e.section, [...(out.get(e.section) ?? []), e]);
  return [...out];
}

const NO_EXTRA: PaletteEntry[] = [];
const NO_HIDDEN: ReadonlySet<ActionId> = new Set();

export type CommandPaletteProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onRun: (id: ActionId) => void;
  /** Rows that come and go with the model, such as the update's (`updatePaletteEntries`), after the fixed ones. */
  extra?: PaletteEntry[];
  /** Actions whose rows the model leaves out for now, such as the reader mode's on a draft (`hiddenPaletteRows`). */
  hidden?: ReadonlySet<ActionId>;
};

/**
 * Every KEYMAP action, with its keys; what M1 cannot run yet is listed,
 * disabled, with the milestone that brings it.
 */
export function CommandPalette({ open, onOpenChange, onRun, extra = NO_EXTRA, hidden = NO_HIDDEN }: CommandPaletteProps) {
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
      <PaletteBody all={all} onRun={run} />
    </CommandDialog>
  );
}

/**
 * The palette's input and rows, mounted with the dialog's popup, so each
 * opening starts with an empty search. With no search the rows sit under
 * their sections; while a search runs they are one list, which cmdk ranks
 * best match first. cmdk 1.1 ranks the rows inside a group but not the
 * groups themselves (it looks a group up by its React id where it stored its
 * value), so grouped rows left the best match under a later section, and
 * Enter ran the first row of an earlier one.
 */
function PaletteBody({ all, onRun }: { all: [string, PaletteEntry[]][]; onRun: (id: ActionId) => void }) {
  const [search, setSearch] = useState("");
  const row = (section: string, e: PaletteEntry) => (
    <CommandItem
      key={`${section}:${e.label}`}
      value={`${section} ${e.label} ${e.keys.join(" ")}`}
      disabled={e.id === null}
      data-testid="palette-item"
      data-label={e.label}
      onSelect={() => {
        if (e.id) onRun(e.id);
      }}
      className="data-[disabled=true]:opacity-100 data-[disabled=true]:text-disabled-foreground"
    >
      <span className="min-w-0 flex-1 truncate">{e.label}</span>
      {e.badge ? <BadgeFor badge={e.badge} /> : null}
      <Keys keys={e.keys} />
    </CommandItem>
  );
  return (
    <Command loop>
      <CommandInput placeholder="Run an action by name…" aria-label="Action name" value={search} onValueChange={setSearch} />
      <CommandList className="max-h-[60vh]">
        <CommandEmpty>No action matches.</CommandEmpty>
        {search.trim() === ""
          ? all.map(([section, entries]) => (
              <CommandGroup key={section} heading={section}>
                {entries.map((e) => row(section, e))}
              </CommandGroup>
            ))
          : all.flatMap(([section, entries]) => entries.map((e) => row(section, e)))}
      </CommandList>
    </Command>
  );
}
