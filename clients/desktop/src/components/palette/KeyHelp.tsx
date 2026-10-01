import { useRef } from "react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { BadgeFor } from "@/components/palette/BadgeFor";
import { Kbd } from "@/components/ui/kbd";
import { bindingFor, GUI_ENTRIES, SECTIONS, type Badge } from "@/keymap/catalog";

export type KeyHelpProps = { open: boolean; onOpenChange: (open: boolean) => void };

type Row = { key: string; label: string; badge: Badge | null };
type Section = { title: string; rows: Row[] };

function badgeOf(section: string, action: string): Badge | null {
  const binding = bindingFor(section, action);
  return "badge" in binding && binding.badge !== "key" ? binding.badge : null;
}

const HELP_SECTIONS: Section[] = [
  ...SECTIONS.map((s) => ({
    title: s.title,
    rows: s.bindings.map((b) => ({ key: b.key, label: b.action, badge: badgeOf(s.title, b.action) })),
  })),
  {
    title: "DESKTOP",
    rows: GUI_ENTRIES.filter((e) => e.keys.length > 0).map((e) => ({
      key: e.keys.join(" / "),
      label: e.label,
      badge: null,
    })),
  },
];

/**
 * The key help overlay, from the same generated KEYMAP data as the TUI's `?`,
 * with a filter that narrows the rows as the user types. Read-only: a row runs
 * nothing.
 */
export function KeyHelp({ open, onOpenChange }: KeyHelpProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={inputRef} className="max-h-[85vh] sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>Keys</DialogTitle>
          <DialogDescription>
            The TUI's bindings, from <code>mp dump-keys</code>, then the desktop client's own. A badge names the
            milestone that brings an action to the desktop client.
          </DialogDescription>
        </DialogHeader>
        <Command label="Filter keys" className="min-h-0 bg-transparent p-0">
          <CommandInput ref={inputRef} placeholder="Filter keys…" />
          <CommandList label="Key bindings" className="max-h-[60vh] pt-2 [&_[cmdk-list-sizer]]:grid [&_[cmdk-list-sizer]]:gap-x-8 [&_[cmdk-list-sizer]]:gap-y-3 md:[&_[cmdk-list-sizer]]:grid-cols-2">
            <CommandEmpty className="md:col-span-2">No matching key</CommandEmpty>
            {HELP_SECTIONS.map((s) => (
              <CommandGroup key={s.title} heading={s.title} className="p-0">
                {s.rows.map((r) => (
                  <CommandItem
                    key={`${r.key}:${r.label}`}
                    value={`${s.title} ${r.label} ${r.key}`}
                    data-testid="help-item"
                    data-label={r.label}
                    className="items-start gap-0 py-0.5"
                  >
                    <span className="w-28 shrink-0 pr-3">
                      <Kbd className="font-mono">{r.key}</Kbd>
                    </span>
                    <span className={`min-w-0 flex-1 ${r.badge ? "text-muted-foreground" : ""}`}>
                      <span className="mr-2">{r.label}</span>
                      {r.badge ? <BadgeFor badge={r.badge} /> : null}
                    </span>
                  </CommandItem>
                ))}
              </CommandGroup>
            ))}
          </CommandList>
        </Command>
      </DialogContent>
    </Dialog>
  );
}
