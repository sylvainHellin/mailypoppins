import { useMemo, useRef, useState, type RefObject } from "react";
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
 * The rows whose section title, key or description contains `query`, case
 * folded, as the TUI's help filter does; a section left empty is dropped.
 */
function filterSections(sections: Section[], query: string): Section[] {
  const q = query.trim().toLowerCase();
  if (!q) return sections;
  return sections
    .map((s) => {
      if (s.title.toLowerCase().includes(q)) return s;
      const rows = s.rows.filter((r) => r.key.toLowerCase().includes(q) || r.label.toLowerCase().includes(q));
      return { ...s, rows };
    })
    .filter((s) => s.rows.length > 0);
}

/**
 * The filter and the rows. It lives inside the dialog's popup, so it unmounts
 * on close and the help opens with an empty filter. The filtering is ours
 * (`shouldFilter={false}`): cmdk's own re-sorts the DOM nodes by score and
 * leaves them shuffled once the field is cleared.
 */
function HelpFilter({ inputRef }: { inputRef: RefObject<HTMLInputElement | null> }) {
  const [query, setQuery] = useState("");
  const shown = useMemo(() => filterSections(HELP_SECTIONS, query), [query]);
  return (
    <Command label="Filter keys" shouldFilter={false} className="min-h-0 bg-transparent p-0">
      <CommandInput ref={inputRef} value={query} onValueChange={setQuery} placeholder="Filter keys…" />
      <CommandList label="Key bindings" className="max-h-[60vh] pt-2 [&_[cmdk-list-sizer]]:grid [&_[cmdk-list-sizer]]:gap-x-8 [&_[cmdk-list-sizer]]:gap-y-3 md:[&_[cmdk-list-sizer]]:grid-cols-2">
        {shown.length === 0 ? <CommandEmpty className="md:col-span-2">No matching key</CommandEmpty> : null}
        {shown.map((s) => (
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
  );
}

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
        <HelpFilter inputRef={inputRef} />
      </DialogContent>
    </Dialog>
  );
}
