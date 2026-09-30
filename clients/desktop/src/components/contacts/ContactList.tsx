import { useEffect, useRef } from "react";
import { ContactRow } from "@/components/contacts/ContactRow";
import type { ContactRow as Contact } from "@/lib/gui-types";

export type ContactListProps = {
  rows: Contact[];
  cursor: string | null;
  /** Moves whenever the keyboard moved the cursor, so the row is scrolled into view. */
  focusSeq: number;
  onSelect: (address: string) => void;
  onOpen: (address: string) => void;
};

/** The ranked contacts, a single-select listbox named "Contacts". */
export function ContactList({ rows, cursor, focusSeq, onSelect, onOpen }: ContactListProps) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    ref.current?.querySelector<HTMLElement>('[data-cursor="true"]')?.scrollIntoView?.({ block: "nearest" });
  }, [cursor, focusSeq]);
  return (
    <div ref={ref} role="listbox" aria-label="Contacts">
      {rows.map((contact, i) => (
        <ContactRow
          key={contact.address}
          contact={contact}
          cursor={contact.address === cursor}
          position={i + 1}
          setSize={rows.length}
          onSelect={onSelect}
          onOpen={onOpen}
        />
      ))}
    </div>
  );
}
