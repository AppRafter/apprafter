// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The store half of what was Settings › Lock (d4a-g Task 4's tests, moved with their code by
// WI-476): where the secrets are and each state's way on, the notes, the slots, this computer.
import { describe, expect, mock, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { HolderHardening } from '../../ipc/generated/HolderHardening';
import type { LockReport } from '../../ipc/generated/LockReport';
import { StoreProtection, type StoreProtectionProps, StoreScope } from './StoreProtection';

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

function show(more: Partial<StoreProtectionProps> = {}) {
  const onAction = mock();
  render(
    <StoreProtection
      os="linux"
      mode="files"
      protectedStore={false}
      info={{
        report: report({ mode: 'files', storeId: null, slots: [], window: { status: 'closed' } }),
        app: HARDENED,
      }}
      problem={null}
      error={null}
      onAction={onAction}
      {...more}
    />,
  );
  return { user: userEvent.setup(), onAction };
}

describe('StoreProtection', () => {
  test('a store in files: where the secrets are, and Protect is offered', async () => {
    const { user, onAction } = show();
    expect(
      screen.getByText("Secrets are in files; the lock only covers this app's window."),
    ).toBeDefined();
    await user.click(screen.getByRole('button', { name: 'Protect…' }));
    expect(onAction).toHaveBeenCalledWith('protect');
  });

  test('locked: Stop protecting, the unlock note, the terminals line and the slots', async () => {
    const { user, onAction } = show({
      mode: 'locked',
      protectedStore: true,
      os: 'windows',
      info: {
        report: report({
          slots: [
            { kind: 'password', label: 'lock password', refused: 'kdf_too_weak' },
            { kind: 'recovery_key', label: 'recovery key', refused: null },
          ],
        }),
        app: HARDENED,
      },
    });
    expect(screen.getByText('Secrets are encrypted; unlocking opens them.')).toBeDefined();
    expect(
      screen.getByText(/instead of Windows Hello, which cannot open encrypted secrets yet/),
    ).toBeDefined();
    expect(
      screen.getByText('Terminals: each one unlocks on its own with apprafter unlock, for 5 min.'),
    ).toBeDefined();
    expect(screen.getByText(/Lock password — refused by this build/)).toBeDefined();
    expect(screen.getByText('Recovery key')).toBeDefined();
    expect(screen.getByText('Changing them comes in a later version.')).toBeDefined();
    await user.click(screen.getByRole('button', { name: 'Stop protecting…' }));
    expect(onAction).toHaveBeenCalledWith('stop');
  });

  test('an interrupted Protect offers Finish and Stop; an interrupted Stop, Finish', async () => {
    const { user, onAction } = show({
      mode: 'locking',
      protectedStore: true,
      info: { report: report({ mode: 'locking' }), app: HARDENED },
    });
    await user.click(screen.getByRole('button', { name: 'Finish…' }));
    expect(onAction).toHaveBeenCalledWith('finish');
    expect(screen.getByRole('button', { name: 'Stop protecting…' })).toBeDefined();
  });

  test('this computer: warnings first, then facts, what the lock does not cover, and the key the app holds', () => {
    show({
      mode: 'locked',
      protectedStore: true,
      info: { report: report(), app: { ...HARDENED, memoryLocked: false } },
    });
    const lines = screen
      .getAllByRole('listitem')
      .map((li) => [li.getAttribute('data-tone'), li.textContent]);
    expect(lines.map(([tone]) => tone)).toEqual(['warn', 'neutral', 'neutral']);
    expect(lines[0]?.[1]).toContain('AppRafter runs as an X11 program here');
    expect(lines.some(([, text]) => text?.includes('sets out to get past the lock'))).toBe(true);
    expect(screen.getByText(/The lock protects secrets at rest\./)).toBeDefined();
    expect(screen.getByText(/the key's memory may be paged to disk/)).toBeDefined();
    expect(screen.getByText(/While AppRafter is unlocked it holds this store’s key/)).toBeDefined();
  });

  test('a store in files lists the limits too, and says nothing of a key the app holds', () => {
    show({
      info: {
        report: report({ mode: 'files', storeId: null, slots: [], window: { status: 'closed' } }),
        app: { ...HARDENED, memoryLocked: false },
      },
    });
    expect(screen.getAllByRole('listitem').length).toBeGreaterThan(0);
    expect(screen.queryByText(/While AppRafter is unlocked it holds/)).toBeNull();
    // No key held: its memory is not measured, so nothing is said of it (critic A finding 6).
    expect(screen.queryByText(/the key's memory may be paged to disk/)).toBeNull();
  });

  test('a lock state that cannot be used is said with why, never as encrypted, and offers nothing', () => {
    const problem = {
      code: 'apprafter::secrets::store_file_unreadable',
      message: 'the lock state of the target store at ~/s cannot be read',
      help: 'Restore store.yaml from a backup.',
      causes: [],
      fields: {},
    };
    show({
      mode: 'locked',
      protectedStore: true,
      problem,
      info: { report: report(), app: HARDENED },
    });
    expect(screen.getByText("This target store's lock state cannot be used.")).toBeDefined();
    expect(
      screen.getByText('the lock state of the target store at ~/s cannot be read'),
    ).toBeDefined();
    expect(screen.queryByText('Secrets are encrypted; unlocking opens them.')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Stop protecting…' })).toBeNull();
  });

  test('a report still being read says so; one that failed shows its error, and Protect still works', async () => {
    const { user, onAction } = show({ info: null });
    expect(screen.getByText('Reading…')).toBeDefined();
    await user.click(screen.getByRole('button', { name: 'Protect…' }));
    expect(onAction).toHaveBeenCalledWith('protect');
    const error = {
      code: 'apprafter::secrets::vault_unreadable',
      message: 'the vault cannot be read',
      help: 'Restore it from a backup.',
      causes: [],
      fields: {},
    };
    show({ info: null, error, mode: 'locked' });
    expect(screen.getByText('the vault cannot be read')).toBeDefined();
  });
});

describe('StoreScope (WI-476)', () => {
  test('the store, and every target it covers: counted, while read, none yet', () => {
    const { rerender } = render(
      <StoreScope
        store="~/.config/apprafter"
        targets={['broken', 'lab', 'prod-eu']}
        error={null}
      />,
    );
    expect(screen.getByText('Target store ~/.config/apprafter')).toBeDefined();
    expect(screen.getByText('Covers 3 targets: broken, lab, prod-eu')).toBeDefined();
    rerender(<StoreScope store="~/s" targets={null} error={null} />);
    expect(screen.getByText('Reading…')).toBeDefined();
    rerender(<StoreScope store="~/s" targets={[]} error={null} />);
    expect(screen.getByText('No targets in this store yet.')).toBeDefined();
  });

  test('targets that cannot be listed say why, and the store is still named', () => {
    const error = {
      code: 'apprafter::io',
      message: 'the targets cannot be read',
      help: null,
      causes: [],
      fields: {},
    };
    render(<StoreScope store="~/s" targets={null} error={error} />);
    expect(screen.getByText('Target store ~/s')).toBeDefined();
    expect(screen.getByText('the targets cannot be read')).toBeDefined();
  });
});
