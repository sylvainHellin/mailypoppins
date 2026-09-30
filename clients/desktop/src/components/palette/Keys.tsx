import { Kbd } from "@/components/ui/kbd";

/** A KEYMAP key cell ("gg / G", "Ctrl+p"), as the TUI help prints it. */
export function Keys({ keys }: { keys: string[] }) {
  if (keys.length === 0) return null;
  return (
    <span className="flex shrink-0 items-center gap-1">
      {keys.map((k) => (
        <Kbd key={k} className="font-mono">
          {k}
        </Kbd>
      ))}
    </span>
  );
}
