import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { BadgeFor } from "@/components/palette/BadgeFor";
import { Kbd } from "@/components/ui/kbd";
import { bindingFor, SECTIONS } from "@/keymap/catalog";

export type KeyHelpProps = { open: boolean; onOpenChange: (open: boolean) => void };

/** The key help overlay, from the same generated KEYMAP data as the TUI's `?`. */
export function KeyHelp({ open, onOpenChange }: KeyHelpProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>Keys</DialogTitle>
          <DialogDescription>
            The TUI's bindings, from <code>mp dump-keys</code>. A badge names the milestone that brings an action to
            the desktop client.
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-x-8 gap-y-5 md:grid-cols-2">
          {SECTIONS.map((s) => (
            <section key={s.title} aria-labelledby={`keys-${s.title}`}>
              <h3 id={`keys-${s.title}`} className="mb-2 text-xs font-semibold tracking-wide text-muted-foreground">
                {s.title}
              </h3>
              <table className="w-full text-sm">
                <tbody>
                  {s.bindings.map((b) => {
                    const binding = bindingFor(s.title, b.action);
                    const badge = "badge" in binding && binding.badge !== "key" ? binding.badge : null;
                    return (
                      <tr key={`${b.key}:${b.action}`} className="align-top">
                        <th scope="row" className="w-28 py-0.5 pr-3 text-left font-normal">
                          <Kbd className="font-mono">{b.key}</Kbd>
                        </th>
                        <td className={`py-0.5 ${badge ? "text-disabled-foreground" : ""}`}>
                          <span className="mr-2">{b.action}</span>
                          {badge ? <BadgeFor badge={badge} /> : null}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </section>
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}
