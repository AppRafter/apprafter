// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The ends of confirmed plans whose screen was gone when they ended: a lock unmounted it (Rust
// runs a confirmed plan on through a lock), or its tab closed. Each waits here, outside the shell
// a lock unmounts, until the shell shows it (shell/EndedAway.tsx), and its operation is discarded
// only then: an end is never dropped unseen. And what a read warned of that its screen went before
// showing (the doctor's sweep of decrypted kubeconfig copies): its operation is gone already.
import { useSyncExternalStore } from 'react';
import type { OpId } from './generated/OpId';

export interface EndedAway {
  /** The operation to discard once it is shown; null for a read's warning, whose is gone. */
  readonly opId: OpId | null;
  /** What happened, in a line: "Add target lab failed: …", "Rename prod: done.". */
  readonly text: string;
  /** A failure or a cancel: the notice is not a success. */
  readonly failed: boolean;
}

let list: readonly EndedAway[] = [];
const listeners = new Set<() => void>();

function publish(next: readonly EndedAway[]) {
  list = next;
  for (const listener of listeners) listener();
}

export function keepEndedAway(entry: EndedAway): void {
  publish([...list, entry]);
}

/** The shell showed it: it goes. */
export function takeEndedAway(shown: EndedAway): void {
  publish(list.filter((entry) => entry !== shown));
}

export function endedAwaySnapshot(): readonly EndedAway[] {
  return list;
}

function watch(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useEndedAway(): readonly EndedAway[] {
  return useSyncExternalStore(watch, endedAwaySnapshot);
}

/** Forget everything; for tests. */
export function resetEndedAway(): void {
  publish([]);
}
