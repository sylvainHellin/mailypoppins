import { memo } from "react";
import { contactName, RANK_ONLY_SCORE } from "@/app/contacts";
import type { ContactRow as Contact } from "@/lib/gui-types";

export type ContactRowProps = {
  contact: Contact;
  cursor: boolean;
  position: number;
  setSize: number;
  onSelect: (address: string) => void;
  onOpen: (address: string) => void;
};

/**
 * One ranked contact, the TUI's contacts row: the name (the address when it
 * has none), the address, how often the user wrote to it (To, Cc) and heard
 * from it, and the match score of a query (an empty query ranks without one).
 */
export const ContactRow = memo(function ContactRow({ contact, cursor, position, setSize, onSelect, onOpen }: ContactRowProps) {
  const name = contactName(contact);
  const named = contact.display_name.trim() !== "";
  const counts = `to ${contact.sent_to}, cc ${contact.sent_cc}, received ${contact.received}`;
  const scored = contact.score !== RANK_ONLY_SCORE;
  return (
    <div
      role="option"
      id={`contact-${contact.address}`}
      aria-selected={cursor}
      aria-posinset={position}
      aria-setsize={setSize}
      aria-label={named ? `${name}, ${contact.address}, ${counts}` : `${contact.address}, ${counts}`}
      tabIndex={cursor ? 0 : -1}
      data-roving={cursor ? "active" : undefined}
      data-cursor={cursor || undefined}
      data-address={contact.address}
      onClick={() => onSelect(contact.address)}
      onDoubleClick={() => onOpen(contact.address)}
      className={`flex cursor-default items-center gap-3 border-b border-border px-3 py-2 text-sm outline-none select-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring ${
        cursor ? "bg-selection text-selection-foreground" : "hover:bg-accent/60"
      }`}
    >
      <span className="min-w-0 flex-1">
        <span className="block truncate">{name}</span>
        {named ? <span className="block truncate text-xs text-muted-foreground">{contact.address}</span> : null}
      </span>
      <span data-slot="contact-counts" className="shrink-0 text-xs tabular-nums text-muted-foreground" title="Sent to, sent in Cc, received">
        {counts}
      </span>
      {scored ? (
        <span data-slot="contact-score" className="w-12 shrink-0 text-right text-xs tabular-nums text-muted-foreground" title="Match score">
          {contact.score}
        </span>
      ) : null}
    </div>
  );
});
