import { useMemo } from "react";
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

export type CommandPaletteProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onRun: (id: ActionId) => void;
  /** Rows that come and go with the model, such as the update's (`updatePaletteEntries`), after the fixed ones. */
  extra?: PaletteEntry[];
};

/**
 * Every KEYMAP action, with its keys; what M1 cannot run yet is listed,
 * disabled, with the milestone that brings it.
 */
export function CommandPalette({ open, onOpenChange, onRun, extra = NO_EXTRA }: CommandPaletteProps) {
  const all = useMemo(() => groups([...paletteEntries(), ...GUI_ENTRIES, ...extra]), [extra]);
  return (
    <CommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Command palette"
      description="Run an action by name"
      className="sm:max-w-xl"
    >
      <Command loop>
        <CommandInput placeholder="Run an action by name…" aria-label="Action name" />
        <CommandList className="max-h-[60vh]">
          <CommandEmpty>No action matches.</CommandEmpty>
          {all.map(([section, entries]) => (
            <CommandGroup key={section} heading={section}>
              {entries.map((e) => (
                <CommandItem
                  key={`${section}:${e.label}`}
                  value={`${section} ${e.label} ${e.keys.join(" ")}`}
                  disabled={e.id === null}
                  data-testid="palette-item"
                  data-label={e.label}
                  onSelect={() => {
                    if (!e.id) return;
                    const id = e.id;
                    onOpenChange(false);
                    // Run after the dialog closes, so focus moves where the action puts it.
                    setTimeout(() => onRun(id), 0);
                  }}
                  className="data-[disabled=true]:opacity-100 data-[disabled=true]:text-disabled-foreground"
                >
                  <span className="min-w-0 flex-1 truncate">{e.label}</span>
                  {e.badge ? <BadgeFor badge={e.badge} /> : null}
                  <Keys keys={e.keys} />
                </CommandItem>
              ))}
            </CommandGroup>
          ))}
        </CommandList>
      </Command>
    </CommandDialog>
  );
}
