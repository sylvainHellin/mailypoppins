import { Fragment } from "react";
import { seenDate } from "@/app/contacts";
import type { ContactRow as Contact } from "@/lib/gui-types";

/**
 * The cursor contact, the TUI's contacts detail pane: the name ("(no name)"
 * when no message named one), the address, how often the user wrote to it and
 * heard from it, and the dates a message first and last named it.
 */
export function ContactDetail({ contact }: { contact: Contact }) {
  const name = contact.display_name.trim() || "(no name)";
  const fields: [string, string][] = [
    ["Email", contact.address],
    ["Interactions", `sent-to ${contact.sent_to}, cc ${contact.sent_cc}, received ${contact.received}`],
    ["First seen", seenDate(contact.first_seen)],
    ["Last seen", seenDate(contact.last_seen)],
  ];
  return (
    <aside
      aria-label="Contact details"
      data-slot="contact-detail"
      className="hidden w-72 shrink-0 overflow-y-auto border-l border-border p-4 text-sm md:block"
    >
      <h3 className="truncate font-semibold">{name}</h3>
      <dl className="mt-3 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1">
        {fields.map(([label, value]) => (
          <Fragment key={label}>
            <dt className="text-muted-foreground">{label}</dt>
            <dd className="truncate tabular-nums" title={value}>
              {value}
            </dd>
          </Fragment>
        ))}
      </dl>
    </aside>
  );
}
