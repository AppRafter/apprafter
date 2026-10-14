// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { HolderHardening } from '../../ipc/generated/HolderHardening';
import type { LockReport } from '../../ipc/generated/LockReport';
import type { Settings } from '../../ipc/generated/Settings';
import { authInfo, settings } from '../../test/fixtures';
import { LockSection, type LockSectionProps } from './LockSection';

const HARDENED: HolderHardening = {
  nonDumpable: true,
  denyAttach: null,
  coreOff: true,
  memoryLocked: true,
  processDacl: null,
};

function report(more: Partial<LockReport> = {}): LockReport {
  return {
    store: '~/.config/apprafter',
    storeId: '3f9c0a1b7d2e4c56',
    targets: ['prod'],
    mode: 'locked',
    slots: [
      { kind: 'password', label: 'lock password', refused: null },
      { kind: 'recovery_key', label: 'recovery key', refused: null },
    ],
    windowMinutes: 5,
    window: { status: 'app' },
    limits: [{ kind: 'secrets_in_use' }, { kind: 'x11_session' }, { kind: 'same_user_programs' }],
    ...more,
  };
}

function show(more: Partial<LockSectionProps> = {}) {
  const onSave = mock((_change: Partial<Settings>) => {});
  const onLock = mock();
  const onAction = mock();
  render(
    <LockSection
      settings={settings()}
      os="linux"
      auth={authInfo()}
      sessionEvents={{ lock: true, sleep: true }}
      mode="files"
      protectedStore={false}
      info={{
        report: report({ mode: 'files', storeId: null, slots: [], window: { status: 'closed' } }),
        app: HARDENED,
      }}
      problem={null}
      error={null}
      onSave={onSave}
      onLock={onLock}
      onAction={onAction}
      {...more}
    />,
  );
  return { user: userEvent.setup(), onSave, onLock, onAction };
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
      mode: 'locked',
      protectedStore: true,
      info: { report: report(), app: HARDENED },
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
      mode: 'locked',
      protectedStore: true,
      info: { report: report(), app: HARDENED },
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

  test('the app lock rows work while the store’s report is still read', async () => {
    const { user, onSave } = show({ info: null });
    await user.click(sw('Lock when the app starts'));
    expect(onSave).toHaveBeenCalled();
  });
});
