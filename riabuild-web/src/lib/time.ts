import { useSyncExternalStore } from "react";

export function formatTime(timestamp: number): string {
  if (!timestamp) return "never";
  return new Date(timestamp).toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/**
 * The wall clock as an external store.
 *
 * "Has this session expired?" is a fact about the world that changes without
 * anything in React changing, which is exactly what `useSyncExternalStore` is
 * for. Snapshots are quantised to the tick so React sees a stable value between
 * ticks instead of a new number on every render.
 */
export function useNow(tickMs = 30_000): number {
  return useSyncExternalStore(
    (onStoreChange) => {
      const id = setInterval(onStoreChange, tickMs);
      return () => clearInterval(id);
    },
    () => Math.floor(Date.now() / tickMs) * tickMs,
    () => 0,
  );
}
