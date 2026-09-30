import type { ReactNode } from "react";

/** A full-window state screen: one card, centred on the canvas. */
export function ScreenFrame({ title, icon, children }: { title: string; icon: ReactNode; children: ReactNode }) {
  return (
    <main className="flex min-h-svh items-center justify-center bg-sidebar p-6" aria-labelledby="screen-title">
      <section className="flex w-full max-w-lg flex-col gap-4 rounded-xl border border-border bg-card p-6 text-card-foreground shadow-sm">
        <div className="flex items-center gap-3">
          {icon}
          <h1 id="screen-title" className="text-lg font-semibold">
            {title}
          </h1>
        </div>
        {children}
      </section>
    </main>
  );
}

export function PathRow({ label, value }: { label: string; value: string | null | undefined }) {
  return (
    <>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 font-mono text-xs break-all">{value || "unknown"}</dd>
    </>
  );
}
