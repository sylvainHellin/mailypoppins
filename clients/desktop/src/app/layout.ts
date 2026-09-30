// The adaptive layout: wide (sidebar + list + reader), medium (icon rail +
// list + reader), narrow (one view at a time).

import { useSyncExternalStore } from "react";
import type { Layout } from "@/app/state";

export const WIDE_MIN = 1100;
export const MEDIUM_MIN = 760;

const WIDE_QUERY = `(min-width: ${WIDE_MIN}px)`;
const MEDIUM_QUERY = `(min-width: ${MEDIUM_MIN}px)`;

export function layoutFor(width: number): Layout {
  if (width >= WIDE_MIN) return "wide";
  if (width >= MEDIUM_MIN) return "medium";
  return "narrow";
}

function snapshot(): Layout {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return "wide";
  if (window.matchMedia(WIDE_QUERY).matches) return "wide";
  if (window.matchMedia(MEDIUM_QUERY).matches) return "medium";
  return "narrow";
}

function subscribe(onChange: () => void): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return () => {};
  const queries = [window.matchMedia(WIDE_QUERY), window.matchMedia(MEDIUM_QUERY)];
  for (const q of queries) q.addEventListener("change", onChange);
  window.addEventListener("resize", onChange);
  return () => {
    for (const q of queries) q.removeEventListener("change", onChange);
    window.removeEventListener("resize", onChange);
  };
}

export function useLayout(): Layout {
  return useSyncExternalStore(subscribe, snapshot, () => "wide");
}
