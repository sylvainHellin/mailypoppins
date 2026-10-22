// The folder picker `gf` opens (ticket 0141), the TUI's `gf`: every mailbox of
// the selected account by name, which is how a folder past the nine number
// keys is reached once the daemon lists every folder the server has.

import { useMemo } from "react";
import {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { MAILBOX_ICON, kindOfRole } from "@/components/sidebar/icons";
import { useAppState, useDispatch } from "@/app/store";
import type { AppState } from "@/app/state";
import type { MailboxKind } from "@/lib/gui-types";

export type FolderPickerProps = { open: boolean; onOpenChange: (open: boolean) => void };

type Entry = { slug: string; label: string; kind: MailboxKind; server: string | null };

/** The selected account's mailboxes, in sidebar order: its listing once read, its bootstrap rows before. */
export function folderEntries(s: AppState): { account: string | null; entries: Entry[] } {
  const account = s.selection.account;
  if (!account) return { account, entries: [] };
  const listing = s.mailboxes[account]?.data;
  const entries: Entry[] = listing
    ? listing.mailboxes.map((m) => ({ slug: m.slug, label: m.label, kind: m.kind, server: m.server }))
    : (s.bootstrap?.snapshot.mailboxes[account] ?? []).map((m) => ({
        slug: m.slug,
        label: m.label,
        kind: kindOfRole(m.role),
        server: m.server ?? null,
      }));
  return { account, entries };
}

/**
 * Type to filter by label or server name, Enter opens the mailbox in the list,
 * Escape cancels.
 */
export function FolderPicker({ open, onOpenChange }: FolderPickerProps) {
  const s = useAppState();
  const dispatch = useDispatch();
  const { account, entries } = useMemo(() => folderEntries(s), [s]);
  const pick = (slug: string) => {
    onOpenChange(false);
    if (account) dispatch({ type: "select_mailbox", account, slug, focus: "list" });
  };
  return (
    <CommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Go to mailbox"
      description="Type to filter the mailboxes, Enter to open, Escape to cancel"
      className="sm:max-w-md"
    >
      <Command loop>
        <CommandInput autoFocus placeholder="Go to mailbox…" aria-label="Mailbox name" />
        <CommandList className="max-h-[50vh]">
          <CommandEmpty>No mailbox matches.</CommandEmpty>
          <CommandGroup heading={account ?? "Mailboxes"}>
            {entries.map((m) => {
              const Icon = MAILBOX_ICON[m.kind];
              return (
                <CommandItem
                  key={m.slug}
                  value={`${m.label} ${m.server ?? ""} ${m.slug}`}
                  data-testid="folder-entry"
                  data-slug={m.slug}
                  onSelect={() => pick(m.slug)}
                >
                  <Icon aria-hidden="true" />
                  <span className="min-w-0 flex-1 truncate">{m.label}</span>
                </CommandItem>
              );
            })}
          </CommandGroup>
        </CommandList>
      </Command>
    </CommandDialog>
  );
}
