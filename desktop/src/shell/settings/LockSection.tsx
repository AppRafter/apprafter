// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Settings › Lock (D.4a §3.14, T74; ADR 0068 §8, reworded 2026-10-14; WI-476): how the app locks.
// The app lock and the target-store lock are one mechanism; this section holds the app lock's rows
// — and while the store's secrets are protected, Require unlock, Lock when the app starts and Lock
// when the computer sleeps or locks shown on and not offered, and Auto-lock without Never (O7):
// the app's unlock is what opens the vault, and its unlocked time ends as a terminal's window
// does. Require unlock then also says how that unlock works (the unlock note). The saved values
// stay as the person chose them and come back when protection is off. The store's own
// protection — where its secrets are, Protect, what opens it, this computer's limits — is the
// Security page's (screens/security): the last row leads there, and its sub says why the rows
// above stay on and where that changes. Presentational: the container (LockSettings) reads
// app_info and the settings.
import { Button } from '../../components/Button';
import { Eyebrow } from '../../components/Eyebrow';
import { LockSimpleIcon, ShieldIcon } from '../../components/icons';
import { SegmentedControl, type SegmentOption } from '../../components/SegmentedControl';
import { SettingRow } from '../../components/SettingRow';
import { Switch } from '../../components/Switch';
import type { AuthInfo } from '../../ipc/generated/AuthInfo';
import type { AutoLock } from '../../ipc/generated/AutoLock';
import type { Os } from '../../ipc/generated/Os';
import type { SecretsMode } from '../../ipc/generated/SecretsMode';
import type { SessionEvents } from '../../ipc/generated/SessionEvents';
import type { Settings } from '../../ipc/generated/Settings';
import { authPrompt } from '../../state/auth';
import { lockOff, noAuthNotice } from '../../state/lock';
import {
  BIOMETRICS_PROTECTED_SUB,
  LOCK_EYEBROW,
  NEVER_PROTECTED,
  NEVER_SAVED_NOTE,
  OPEN_SECURITY,
  PINNED_SUB,
  SECURITY_ROW,
  securityRowSub,
  unlockNote,
} from '../../state/lockText';
import { shortcutHint } from '../../state/shortcuts';

export interface LockSectionProps {
  readonly settings: Settings;
  readonly os: Os;
  readonly auth: AuthInfo;
  readonly sessionEvents: SessionEvents;
  /** Where the store keeps its secrets (AppInfo.secrets.mode): the Security row's sub. */
  readonly mode: SecretsMode;
  /** The app's unlock opens the store's vault (AppInfo.secrets.protected): the pins apply. */
  readonly protectedStore: boolean;
  /** The store's lock state cannot be used (AppInfo.secrets.problem): the Security row says so. */
  readonly storeProblem: boolean;
  readonly onSave: (change: Partial<Settings>) => void;
  readonly onLock: () => void;
  /** Shows the Security page (Settings closes): the store's own protection is there. */
  readonly onOpenSecurity: () => void;
}

const AUTO_LOCK = [
  { value: '5', label: '5 min' },
  { value: '10', label: '10 min' },
  { value: '30', label: '30 min' },
  { value: 'never', label: 'Never' },
] as const satisfies readonly SegmentOption<AutoLock>[];

/** The lock-on-sleep row's note: what the OS tells the app of its session, and what it does not. */
export function sessionNote({ lock, sleep }: SessionEvents): string {
  if (lock && sleep) return 'When the screen locks or the computer goes to sleep.';
  if (lock) return 'AppRafter is told when the screen locks, not when the computer sleeps.';
  if (sleep) return 'AppRafter is told when the computer sleeps, not when the screen locks.';
  return 'This computer does not tell AppRafter when it locks or sleeps.';
}

export function LockSection(p: LockSectionProps) {
  const pinned = p.protectedStore;
  const noAuth = !p.auth.available;
  const off = lockOff(p.auth.available, p.settings.lockEnabled, pinned) !== null;
  const method = p.auth.method;
  const notice = noAuthNotice(p.auth.available, pinned);
  const savedNever = p.settings.autoLock === 'never';
  // O7: while protected Never is not offered; a saved Never stays saved and reads as 30 min.
  const autoLock: AutoLock = pinned && savedNever ? '30' : p.settings.autoLock;
  const options = AUTO_LOCK.map((o) =>
    o.value === 'never' && pinned ? { ...o, disabled: true, title: NEVER_PROTECTED } : o,
  );
  const requireSub = pinned
    ? `${PINNED_SUB} ${unlockNote(p.os)}`
    : method === null
      ? 'Ask for the computer’s owner before showing any cluster.'
      : `Ask for ${authPrompt(method)} before showing any cluster.`;

  return (
    <>
      <Eyebrow>{LOCK_EYEBROW}</Eyebrow>
      {notice !== null && (
        <p className="settings-banner" role="note">
          {notice}
        </p>
      )}
      <SettingRow
        label="Require unlock"
        disabled={!pinned && noAuth}
        sub={requireSub}
        control={
          <Switch
            label="Require unlock"
            checked={pinned || (p.settings.lockEnabled && !noAuth)}
            disabled={pinned || noAuth}
            onChange={(lockEnabled) => p.onSave({ lockEnabled })}
          />
        }
      />
      <SettingRow
        label="Lock when the app starts"
        sub={pinned ? PINNED_SUB : 'The app opens on the lock screen.'}
        disabled={off}
        control={
          <Switch
            label="Lock when the app starts"
            checked={pinned || p.settings.lockOnStart}
            disabled={pinned || off}
            onChange={(lockOnStart) => p.onSave({ lockOnStart })}
          />
        }
      />
      <SettingRow
        label="Lock when the computer sleeps or locks"
        sub={
          pinned ? `${PINNED_SUB} ${sessionNote(p.sessionEvents)}` : sessionNote(p.sessionEvents)
        }
        disabled={off}
        control={
          <Switch
            label="Lock when the computer sleeps or locks"
            checked={pinned || p.settings.lockOnSleep}
            disabled={pinned || off}
            onChange={(lockOnSleep) => p.onSave({ lockOnSleep })}
          />
        }
      />
      {p.auth.biometricsChoice && (
        <SettingRow
          label="Prefer biometrics"
          sub={pinned ? BIOMETRICS_PROTECTED_SUB : 'Fingerprint, face or PIN before the password.'}
          disabled={off}
          control={
            <Switch
              label="Prefer biometrics"
              checked={p.settings.hello}
              disabled={off}
              onChange={(hello) => p.onSave({ hello })}
            />
          }
        />
      )}
      <SettingRow
        label="Auto-lock after inactivity"
        sub={
          pinned && savedNever ? NEVER_SAVED_NOTE : 'How long the app waits before it locks itself.'
        }
        disabled={off}
        control={
          <SegmentedControl
            ariaLabel="Auto-lock after inactivity"
            font="mono"
            value={autoLock}
            options={options}
            disabled={off}
            onChange={(value) => p.onSave({ autoLock: value })}
          />
        }
      />
      <SettingRow
        label="Lock now"
        sub={`Also ${shortcutHint('lock', p.os)}.`}
        disabled={off}
        control={
          <Button size={28} icon={LockSimpleIcon} disabled={off} onClick={p.onLock}>
            Lock
          </Button>
        }
      />
      <SettingRow
        label={SECURITY_ROW}
        sub={securityRowSub(p.mode, pinned, p.storeProblem)}
        control={
          <Button size={28} icon={ShieldIcon} onClick={p.onOpenSecurity}>
            {OPEN_SECURITY}
          </Button>
        }
      />
    </>
  );
}
