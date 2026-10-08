// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What Rust pushes to the page: `lock-changed` is the only event.
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { LOCK_CHANGED } from './generated/events';
import type { LockState } from './generated/LockState';

/** Calls `handler` with the new state on every lock transition; resolves with the unlisten. */
export function onLockChanged(handler: (state: LockState) => void): Promise<UnlistenFn> {
  return listen<LockState>(LOCK_CHANGED, (event) => handler(event.payload));
}
