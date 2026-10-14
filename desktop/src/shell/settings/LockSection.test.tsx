// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { Settings } from '../../ipc/generated/Settings';
import {
  ACTION_LABELS,
  LIMITS_TITLE,
  OPEN_SECURITY,
  PINNED_SUB,
  SECURITY_ROW_SUB,
  securityRowSub,
  unlockNote,
} from '../../state/lockText';
import { authInfo, settings } from '../../test/fixtures';
import { LockSection, type LockSectionProps } from './LockSection';

function show(more: Partial<LockSectionProps> = {}) {
  const onSave = mock((_change: Partial<Settings>) => {});
  const onLock = mock();
  const onOpenSecurity = mock();
  render(
    <LockSection
      settings={settings()}
      os="linux"
      auth={authInfo()}
      sessionEvents={{ lock: true, sleep: true }}
      mode="files"
      protectedStore={false}
      storeProblem={false}
      onSave={onSave}
      onLock={onLock}
      onOpenSecurity={onOpenSecurity}
      {...more}
    />,
  );
  return { user: userEvent.setup(), onSave, onLock, onOpenSecurity };
}

const pinnedSwitches = [
  'Require unlock',
  'Lock when the app starts',
  'Lock when the computer sleeps or locks',
];
const sw = (name: string) => screen.getByRole('switch', { name }) as HTMLButtonElement;

describe('LockSection', () => {
  test('a store in files: the app lock rows as before', async () => {
    const { user, onSave } = show({ settings: settings({ lockOnStart: false }) });
    // The eyebrow says Lock (D1); the Lock now button says it too.
    expect(screen.getAllByText('Lock').length).toBeGreaterThanOrEqual(2);
    expect(sw('Lock when the app starts').getAttribute('aria-checked')).toBe('false');
    await user.click(sw('Lock when the app starts'));
    expect(onSave).toHaveBeenCalledWith({ lockOnStart: true });
    expect(screen.getByRole('radio', { name: 'Never' }).getAttribute('aria-disabled')).toBeNull();
  });

  test('protected: the three switches stay on, Never is not offered, the saved choices are kept', async () => {
    const { user, onSave } = show({
      protectedStore: true,
      settings: settings({
        lockEnabled: false,
        lockOnStart: false,
        lockOnSleep: false,
        autoLock: 'never',
      }),
    });
    for (const name of pinnedSwitches) {
      expect(sw(name).getAttribute('aria-checked')).toBe('true');
      expect(sw(name).disabled).toBe(true);
      await user.click(sw(name));
    }
    expect(onSave).not.toHaveBeenCalled();
    expect(
      screen.getAllByText(/Always on while secrets are protected: unlocking is what opens them\./),
    ).toHaveLength(3);
    const never = screen.getByRole('radio', { name: 'Never' });
    expect(never.getAttribute('aria-disabled')).toBe('true');
    expect(never.getAttribute('title')).toBe('Not while secrets are protected.');
    expect(screen.getByRole('radio', { name: '30 min' }).getAttribute('aria-checked')).toBe('true');
    expect(
      screen.getByText(/Your choice of Never comes back when protection is off/),
    ).toBeDefined();
  });

  test('protected with no system authentication: the lock is still on, and the banner says how it confirms', () => {
    show({
      protectedStore: true,
      auth: { ...authInfo(), available: false, method: null, unavailable: 'no_backend' },
    });
    expect(sw('Require unlock').getAttribute('aria-checked')).toBe('true');
    expect(screen.getByRole('note').textContent).toBe(
      'This computer offers no system authentication AppRafter can use: confirmations ask for the lock password.',
    );
    expect((screen.getByRole('button', { name: 'Lock' }) as HTMLButtonElement).disabled).toBe(
      false,
    );
  });

  test('a row that opens the Security page, and nothing of the store here (WI-476)', async () => {
    const { user, onOpenSecurity } = show({ mode: 'locked', protectedStore: true });
    expect(screen.getByText('Target store protection')).toBeDefined();
    await user.click(screen.getByRole('button', { name: OPEN_SECURITY }));
    expect(onOpenSecurity).toHaveBeenCalledTimes(1);
    for (const label of Object.values(ACTION_LABELS)) {
      expect(screen.queryByRole('button', { name: label })).toBeNull();
    }
    expect(screen.queryByText(LIMITS_TITLE)).toBeNull();
  });

  // Critic M2: the pins stay here, so their reason and their remedy are said here too.
  test("the row's sub: why the rows above stay on and where that changes, per state", () => {
    show({ mode: 'locked', protectedStore: true });
    expect(screen.getByText(securityRowSub('locked', true, false))).toBeDefined();
    cleanup();
    show({ mode: 'locking', protectedStore: true });
    expect(screen.getByText(securityRowSub('locking', true, false))).toBeDefined();
    cleanup();
    show({ mode: 'locked', protectedStore: false, storeProblem: true });
    expect(screen.getByText(securityRowSub('locked', false, true))).toBeDefined();
    cleanup();
    show();
    expect(screen.getByText(SECURITY_ROW_SUB)).toBeDefined();
  });

  // Critic m4: how the app now unlocks is the app lock's own behaviour, so Settings says it.
  test('protected: Require unlock says how the app unlocks now, on every OS', () => {
    for (const os of ['linux', 'macos', 'windows'] as const) {
      show({ os, mode: 'locked', protectedStore: true });
      expect(screen.getByText(`${PINNED_SUB} ${unlockNote(os)}`)).toBeDefined();
      cleanup();
    }
    show();
    expect(screen.queryByText(/instead of your system password/)).toBeNull();
  });
});
