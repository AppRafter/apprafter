// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Generated from desktop/ipc/src/lock.rs, app_info.rs and quit.rs by `just desktop-ipc-types`. Do not edit.

/** Emitted on every lock transition, with the new `LockState`. */
export const LOCK_CHANGED = 'lock-changed' as const;

/** Emitted when a re-read finds the target store changed; no payload: read `app_info` again. */
export const STORE_CHANGED = 'store-changed' as const;

/** Emitted when a quit begins with operations running, with `Quitting`. */
export const QUITTING = 'quitting' as const;
