import { beforeEach, describe, expect, it } from "vitest";
import { forgetReads, joinRead } from "@/lib/inflight";

/** Reads that answer when the test says so: `answer(i, v)` settles the i-th read asked. */
function gated<T>() {
  const pending: { resolve: (v: T) => void; reject: (e: unknown) => void }[] = [];
  const read = () => new Promise<T>((resolve, reject) => pending.push({ resolve, reject }));
  return {
    read,
    asked: () => pending.length,
    answer: (i: number, v: T) => pending[i].resolve(v),
    refuse: (i: number, e: unknown) => pending[i].reject(e),
  };
}

beforeEach(() => forgetReads());

describe("joinRead", () => {
  it("joins a second reader of the key to the read on its way, and reads afresh once it settled", async () => {
    const r = gated<string>();
    const first = joinRead("k", r.read);
    const second = joinRead("k", r.read);
    expect(second).toBe(first);
    expect(r.asked()).toBe(1);
    r.answer(0, "one");
    await expect(second).resolves.toBe("one");
    const third = joinRead("k", r.read);
    expect(r.asked()).toBe(2);
    r.answer(1, "two");
    await expect(third).resolves.toBe("two");
  });

  it("keeps the keys apart", () => {
    const r = gated<string>();
    void joinRead("a", r.read);
    void joinRead("b", r.read);
    expect(r.asked()).toBe(2);
  });

  it("does not keep a refused read", async () => {
    const r = gated<string>();
    const first = joinRead("k", r.read);
    r.refuse(0, new Error("no"));
    await expect(first).rejects.toThrow("no");
    void joinRead("k", r.read);
    expect(r.asked()).toBe(2);
  });

  it("forgets the reads under a prefix, and a forgotten read that settles does not clear the newer one", async () => {
    const r = gated<string>();
    const old = joinRead("text:1", r.read);
    const kept = joinRead("other", r.read);
    forgetReads("text:");
    expect(joinRead("other", r.read)).toBe(kept);
    const newer = joinRead("text:1", r.read);
    expect(newer).not.toBe(old);
    expect(r.asked()).toBe(3);
    r.answer(0, "stale");
    await old;
    expect(joinRead("text:1", r.read)).toBe(newer);
    expect(r.asked()).toBe(3);
  });
});
