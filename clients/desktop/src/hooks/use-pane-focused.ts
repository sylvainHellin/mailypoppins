// The pane that holds the model's focus (`state.focus`) carries
// `data-focused="true"` on its `data-pane` root, which index.css draws as a
// one-pixel ring line inside its edge (clients/desktop/docs/shell.md, "Focus
// order"); the other panes carry no attribute.

import { useAppState } from "@/app/store";
import type { Pane } from "@/app/state";

export function usePaneFocused(pane: Pane): "true" | undefined {
  return useAppState().focus === pane ? "true" : undefined;
}
