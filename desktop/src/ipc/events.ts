// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What Rust pushes to the page: `lock-changed` on every lock transition, and `quitting` when a
// quit begins with operations to wait for.
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { LOCK_CHANGED, QUITTING } from './generated/events';
import type { LockState } from './generated/LockState';
import type { Quitting } from './generated/Quitting';

/** Calls `handler` with the new state on every lock transition; resolves with the unlisten. */
export function onLockChanged(handler: (state: LockState) => void): Promise<UnlistenFn> {
  return listen<LockState>(LOCK_CHANGED, (event) => handler(event.payload));
}

/** Calls `handler` when a quit begins with operations running; resolves with the unlisten. */
export function onQuitting(handler: (quitting: Quitting) => void): Promise<UnlistenFn> {
  return listen<Quitting>(QUITTING, (event) => handler(event.payload));
}
