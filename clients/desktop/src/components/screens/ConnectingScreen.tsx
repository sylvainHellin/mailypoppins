import { Skeleton } from "@/components/ui/skeleton";

/** Before the first bootstrap: the shell's silhouette, and what is happening. */
export function ConnectingScreen({ reason }: { reason: string | null }) {
  return (
    <div className="flex min-h-svh bg-sidebar" aria-busy="true">
      <div className="hidden w-64 shrink-0 flex-col gap-2 p-4 md:flex" aria-hidden="true">
        <Skeleton className="h-6 w-32" />
        <Skeleton className="mt-4 h-4 w-20" />
        {Array.from({ length: 5 }, (_, i) => (
          <Skeleton key={i} className="h-7 w-full" />
        ))}
      </div>
      <main className="m-2 flex flex-1 flex-col gap-3 rounded-xl bg-background p-4">
        <p role="status" className="text-sm text-muted-foreground">
          {reason ? `Reconnecting to the mailypoppins daemon: ${reason}` : "Connecting to the mailypoppins daemon…"}
        </p>
        {Array.from({ length: 8 }, (_, i) => (
          <Skeleton key={i} className="h-12 w-full max-w-md" aria-hidden="true" />
        ))}
      </main>
    </div>
  );
}
