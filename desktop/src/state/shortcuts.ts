// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The app's three shortcuts. Mod is Cmd on macOS and Ctrl elsewhere; keys are matched by
// physical key (`code`), so they work on any keyboard layout, a Cyrillic one included.
import type { Os } from '../ipc/generated/Os';

export type ShortcutAction = 'targets' | 'settings' | 'lock';

/** The parts of a KeyboardEvent the shortcuts read. */
export interface KeyLike {
  readonly code: string;
  readonly ctrlKey: boolean;
  readonly metaKey: boolean;
  readonly altKey: boolean;
  readonly shiftKey: boolean;
  readonly target: EventTarget | null;
}

const KEYS: Record<string, { action: ShortcutAction; label: string }> = {
  KeyT: { action: 'targets', label: 'T' },
  Comma: { action: 'settings', label: ',' },
  KeyL: { action: 'lock', label: 'L' },
};

function typingIn(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return (
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLSelectElement ||
    target.isContentEditable
  );
}

/** The action a key press asks for; while typing in a field, only the lock. */
export function shortcutFor(event: KeyLike, os: Os): ShortcutAction | null {
  const mod = os === 'macos' ? event.metaKey : event.ctrlKey;
  const other = os === 'macos' ? event.ctrlKey : event.metaKey;
  if (!mod || other || event.altKey || event.shiftKey) return null;
  const action = Object.hasOwn(KEYS, event.code) ? KEYS[event.code]?.action : undefined;
  if (action === undefined) return null;
  if (action !== 'lock' && typingIn(event.target)) return null;
  return action;
}

/** "⌘T" on macOS, "Ctrl+T" elsewhere. */
export function shortcutHint(action: ShortcutAction, os: Os): string {
  const label = Object.values(KEYS).find((k) => k.action === action)?.label ?? '';
  return os === 'macos' ? `⌘${label}` : `Ctrl+${label}`;
}
