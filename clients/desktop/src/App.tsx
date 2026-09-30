import { TooltipProvider } from "@/components/ui/tooltip";
import { AppShell } from "@/components/shell/AppShell";
import { StoreProvider } from "@/app/store";
import type { AppState } from "@/app/state";

export default function App({ initial }: { initial?: AppState }) {
  return (
    <StoreProvider initial={initial}>
      <TooltipProvider>
        <AppShell />
      </TooltipProvider>
    </StoreProvider>
  );
}
