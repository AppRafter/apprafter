// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What Rust pushes to the page: `lock-changed` on every lock transition, `store-changed` when a
// re-read finds the target store changed (a terminal's `lock on` or `lock off`, a store root
// others can now change: D.4a F17), and `quitting` when a quit begins with operations to wait for.
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { LOCK_CHANGED, QUITTING, STORE_CHANGED } from './generated/events';
import type { LockState } from './generated/LockState';
import type { Quitting } from './generated/Quitting';

/** Calls `handler` with the new state on every lock transition; resolves with the unlisten. */
export function onLockChanged(handler: (state: LockState) => void): Promise<UnlistenFn> {
  return listen<LockState>(LOCK_CHANGED, (event) => handler(event.payload));
}

/**
 * Calls `handler` when Rust's re-read finds the target store changed, locked or not; resolves with
 * the unlisten. No payload: what changed is app_info's to say.
 */
export function onStoreChanged(handler: () => void): Promise<UnlistenFn> {
  return listen(STORE_CHANGED, () => handler());
}

/** Calls `handler` when a quit begins with operations running; resolves with the unlisten. */
export function onQuitting(handler: (quitting: Quitting) => void): Promise<UnlistenFn> {
  return listen<Quitting>(QUITTING, (event) => handler(event.payload));
}
