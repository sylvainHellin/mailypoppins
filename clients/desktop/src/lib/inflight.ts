// One daemon read per key at a time. An effect that reads runs twice on
// mount under React's StrictMode, as every dev build and every test renders
// (main.tsx, src/test/render.tsx): its cleanup's `live = false` drops the
// first answer but not the first request, so the remount asks again. A read
// started through `joinRead` is joined by any reader of the same key while it
// is on its way, the remount included; once it settles, the next reader asks
// the daemon afresh, so nothing is cached here and a refused read is not kept.
// A key names the command and every argument and generation the answer
// depends on.

const reads = new Map<string, Promise<unknown>>();

/** The read under `key`: the one on its way, or `read()` started now. */
export function joinRead<T>(key: string, read: () => Promise<T>): Promise<T> {
  const pending = reads.get(key);
  if (pending) return pending as Promise<T>;
  // A read that `forgetReads` dropped on its way does not clear a newer read under its key.
  const settle = () => {
    if (reads.get(key) === next) reads.delete(key);
  };
  const next = read().then(
    (value) => {
      settle();
      return value;
    },
    (e: unknown) => {
      settle();
      throw e;
    },
  );
  reads.set(key, next);
  return next;
}

/** Drop the reads on their way under keys starting with `prefix` (every read by default); a test starts with none. */
export function forgetReads(prefix = ""): void {
  for (const key of [...reads.keys()]) {
    if (key.startsWith(prefix)) reads.delete(key);
  }
}
