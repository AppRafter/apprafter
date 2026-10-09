// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, test } from 'bun:test';
import { type KeyLike, shortcutFor, shortcutHint } from './shortcuts';

const key = (code: string, mods: Partial<KeyLike> = {}, target?: Element): KeyLike => ({
  code,
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  shiftKey: false,
  target: target ?? document.body,
  ...mods,
});

describe('shortcutFor', () => {
  test('Mod is Cmd on macOS and Ctrl elsewhere', () => {
    expect(shortcutFor(key('KeyT', { metaKey: true }), 'macos')).toBe('targets');
    expect(shortcutFor(key('KeyT', { ctrlKey: true }), 'macos')).toBeNull();
    expect(shortcutFor(key('KeyT', { ctrlKey: true }), 'windows')).toBe('targets');
    expect(shortcutFor(key('KeyT', { metaKey: true }), 'linux')).toBeNull();
  });

  test('T opens Targets, comma opens Settings, L locks', () => {
    expect(shortcutFor(key('KeyT', { ctrlKey: true }), 'linux')).toBe('targets');
    expect(shortcutFor(key('Comma', { ctrlKey: true }), 'linux')).toBe('settings');
    expect(shortcutFor(key('KeyL', { ctrlKey: true }), 'linux')).toBe('lock');
    expect(shortcutFor(key('KeyX', { ctrlKey: true }), 'linux')).toBeNull();
    expect(shortcutFor(key('KeyT'), 'linux')).toBeNull();
  });

  test('another modifier makes it a different shortcut', () => {
    expect(shortcutFor(key('KeyT', { ctrlKey: true, shiftKey: true }), 'windows')).toBeNull();
    expect(shortcutFor(key('KeyL', { ctrlKey: true, altKey: true }), 'windows')).toBeNull();
  });

  test('the physical key counts, whatever the layout types', () => {
    // A Cyrillic layout types another letter on the T key; `code` is still KeyT.
    expect(shortcutFor(key('KeyT', { ctrlKey: true }), 'windows')).toBe('targets');
  });

  test('while typing in a field only the lock fires', () => {
    const input = document.createElement('input');
    const area = document.createElement('textarea');
    expect(shortcutFor(key('KeyT', { ctrlKey: true }, input), 'windows')).toBeNull();
    expect(shortcutFor(key('Comma', { ctrlKey: true }, area), 'windows')).toBeNull();
    expect(shortcutFor(key('KeyL', { ctrlKey: true }, input), 'windows')).toBe('lock');
  });

  test('while a dialog holds the focus only the lock fires', () => {
    const dialog = document.createElement('div');
    dialog.setAttribute('role', 'dialog');
    dialog.setAttribute('aria-modal', 'true');
    const button = document.createElement('button');
    dialog.append(button);
    expect(shortcutFor(key('KeyT', { ctrlKey: true }, button), 'windows')).toBeNull();
    expect(shortcutFor(key('Comma', { ctrlKey: true }, dialog), 'windows')).toBeNull();
    expect(shortcutFor(key('KeyL', { ctrlKey: true }, button), 'windows')).toBe('lock');
  });
});

test('hints read the way each OS writes shortcuts', () => {
  expect(shortcutHint('targets', 'macos')).toBe('⌘T');
  expect(shortcutHint('settings', 'macos')).toBe('⌘,');
  expect(shortcutHint('lock', 'windows')).toBe('Ctrl+L');
  expect(shortcutHint('targets', 'linux')).toBe('Ctrl+T');
});
