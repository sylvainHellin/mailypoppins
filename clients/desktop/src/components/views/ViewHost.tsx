import { CalendarView } from "@/components/calendar/CalendarView";
import { ContactsView } from "@/components/contacts/ContactsView";
import { EmptyView } from "@/components/views/EmptyView";
import type { View } from "@/app/state";

/**
 * The full-pane view in place of the list and the reader. Each M4 unit swaps
 * its view's placeholder for its own component here.
 */
export function ViewHost({ view }: { view: Exclude<View, "mail"> }) {
  switch (view) {
    case "contacts":
      return <ContactsView />;
    case "calendar":
      return <CalendarView />;
    case "settings":
      return <EmptyView view="settings">Settings are not in the desktop client yet.</EmptyView>;
  }
}
