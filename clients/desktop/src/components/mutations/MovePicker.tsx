import { useMemo } from "react";
import { Folder } from "lucide-react";
import {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { moveDestinations } from "@/app/actions";
import type { AppState, MutationDialog } from "@/app/state";

export type MovePickerProps = {
  state: AppState;
  dialog: Extract<MutationDialog, { kind: "move" }> | null;
  onOpenChange: (open: boolean) => void;
  onPick: (slug: string) => void;
};

/**
 * The TUI's `M` picker: the account's mailboxes less Drafts and the one the
 * rows are in, filtered as the name is typed; Enter moves, Escape cancels.
 */
export function MovePicker({ state, dialog, onOpenChange, onPick }: MovePickerProps) {
  const destinations = useMemo(
    () => (dialog ? moveDestinations(state, dialog.account, dialog.source) : []),
    [state, dialog],
  );
  const n = dialog?.targets.length ?? 0;
  const title = n === 1 ? "Move the message to" : `Move ${n} messages to`;
  return (
    <CommandDialog
      open={dialog !== null}
      onOpenChange={onOpenChange}
      title={title}
      description="Type to filter the mailboxes, Enter to move, Escape to cancel"
      className="sm:max-w-md"
    >
      <Command loop>
        <CommandInput autoFocus placeholder={`${title}…`} aria-label="Mailbox name" />
        <CommandList className="max-h-[50vh]">
          <CommandEmpty>No mailbox matches.</CommandEmpty>
          <CommandGroup heading={title}>
            {destinations.map((m) => (
              <CommandItem
                key={m.slug}
                value={`${m.label} ${m.slug}`}
                data-testid="move-destination"
                data-slug={m.slug}
                onSelect={() => onPick(m.slug)}
              >
                <Folder aria-hidden="true" />
                <span className="min-w-0 flex-1 truncate">{m.label}</span>
              </CommandItem>
            ))}
          </CommandGroup>
        </CommandList>
      </Command>
    </CommandDialog>
  );
}
