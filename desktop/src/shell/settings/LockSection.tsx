// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Settings › Lock (D.4a §3.14, T74, T81; ADR 0068 §8): the app lock and the target-store lock as
// one section. (1) The app lock's rows — and while the store's secrets are protected, Require
// unlock, Lock when the app starts and Lock when the computer sleeps or locks shown on and not
// offered, and Auto-lock without Never (O7): the app's unlock is what opens the vault, and its
// unlocked time ends as a terminal's window does. The saved values stay as the person chose them
// and come back when protection is off. (2) Protect secrets: where the store's secrets are, and
// each state's way on. (3) The slots, read-only in D.4a. (4) This computer: every limit the core
// reports, in the app's words, warnings first, what the lock does not cover, and what the app's
// own process could not do to protect the key (rule 1: never mask). Presentational: the
// container (LockSettings) reads lock_info, app_info and the settings, and runs the flows.
import { Button } from '../../components/Button';
import { ErrorPanel } from '../../components/ErrorPanel';
import { Eyebrow } from '../../components/Eyebrow';
import { LockSimpleIcon } from '../../components/icons';
import { SegmentedControl, type SegmentOption } from '../../components/SegmentedControl';
import { SettingRow } from '../../components/SettingRow';
import { Switch } from '../../components/Switch';
import type { AuthInfo } from '../../ipc/generated/AuthInfo';
import type { AutoLock } from '../../ipc/generated/AutoLock';
import type { HolderHardening } from '../../ipc/generated/HolderHardening';
import type { LockReport } from '../../ipc/generated/LockReport';
import type { Os } from '../../ipc/generated/Os';
import type { SecretsMode } from '../../ipc/generated/SecretsMode';
import type { SessionEvents } from '../../ipc/generated/SessionEvents';
import type { Settings } from '../../ipc/generated/Settings';
import type { UiError } from '../../ipc/generated/UiError';
import { authPrompt } from '../../state/auth';
import { lockOff, noAuthNotice } from '../../state/lock';
import {
  ACTION_LABELS,
  APP_HOLDS_KEY,
  AT_REST,
  BIOMETRICS_PROTECTED_SUB,
  hardeningLine,
  LIMITS_TITLE,
  type LimitText,
  LOCK_EYEBROW,
  limitText,
  NEVER_PROTECTED,
  NEVER_SAVED_NOTE,
  PINNED_SUB,
  PROTECT_ACTIONS,
  PROTECT_LABEL,
  type ProtectAction,
  protectSub,
  READING,
  SLOT_NAMES,
  SLOT_REFUSALS,
  SLOTS_LABEL,
  SLOTS_LATER,
  STORE_PROBLEM_SUB,
  terminalsLine,
  unlockNote,
} from '../../state/lockText';
import { shortcutHint } from '../../state/shortcuts';

/**
 * What lock_info answers (the IPC type `LockSectionInfo`, Task 11): the report and the app's own
 * protections.
 */
export interface LockInfoView {
  readonly report: LockReport;
  readonly app: HolderHardening;
}

export interface LockSectionProps {
  readonly settings: Settings;
  readonly os: Os;
  readonly auth: AuthInfo;
  readonly sessionEvents: SessionEvents;
  /** Where the store keeps its secrets (AppInfo.secrets.mode). */
  readonly mode: SecretsMode;
  /** The app's unlock opens the store's vault (AppInfo.secrets.protected): the pins apply. */
  readonly protectedStore: boolean;
  /** lock_info's answer; `null` while it is read, or when `error` says why it is not there. */
  readonly info: LockInfoView | null;
  readonly error: UiError | null;
  /**
   * The store's lock state cannot be used (AppInfo.secrets.problem: an unreadable store.yaml, a
   * lost vault; critic B finding 8): said, with why, in place of the state line and its actions.
   */
  readonly problem: UiError | null;
  readonly onSave: (change: Partial<Settings>) => void;
  readonly onLock: () => void;
  readonly onAction: (action: ProtectAction) => void;
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
    ? PINNED_SUB
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
        label={PROTECT_LABEL}
        sub={p.problem === null ? protectSub(p.mode) : STORE_PROBLEM_SUB}
        control={(p.problem === null ? PROTECT_ACTIONS[p.mode] : []).map((action) => (
          <Button
            key={action}
            size={28}
            variant={action === 'stop' ? 'danger' : 'secondary'}
            onClick={() => p.onAction(action)}
          >
            {ACTION_LABELS[action]}
          </Button>
        ))}
      />
      {p.problem !== null && <ErrorPanel error={p.problem} />}
      {p.mode !== 'files' && (
        <div className="lock-notes">
          {pinned && <p>{unlockNote(p.os)}</p>}
          {p.info !== null && <p>{terminalsLine(p.info.report.windowMinutes)}</p>}
        </div>
      )}
      {p.info !== null && p.mode !== 'files' && p.info.report.slots.length > 0 && (
        <SettingRow
          label={SLOTS_LABEL}
          sub={
            <span className="lock-slots">
              {p.info.report.slots.map((slot) => (
                <span key={`${slot.kind}:${slot.label}`}>
                  {slot.refused === null
                    ? SLOT_NAMES[slot.kind]
                    : `${SLOT_NAMES[slot.kind]} — ${SLOT_REFUSALS[slot.refused]}`}
                </span>
              ))}
              <span>{SLOTS_LATER}</span>
            </span>
          }
        />
      )}

      <div className="lock-limits-head">{LIMITS_TITLE}</div>
      {p.error !== null ? (
        <ErrorPanel error={p.error} />
      ) : p.info === null ? (
        <p className="lock-notes">{READING}</p>
      ) : (
        <Limits info={p.info} holdsKey={p.info.report.window.status === 'app'} />
      )}
    </>
  );
}

/** (4): the limits, warnings first, then what the lock does not cover and the app's own state. */
function Limits({ info, holdsKey }: { info: LockInfoView; holdsKey: boolean }) {
  const lines = info.report.limits
    .map(limitText)
    .filter((line): line is LimitText => line !== null);
  // As `lock status` lists them: the warnings first, each group in the core's order.
  const ordered = [...lines.filter((l) => l.warn), ...lines.filter((l) => !l.warn)];
  const hardening = hardeningLine(info.app, holdsKey);
  return (
    <div className="lock-notes">
      <ul className="lock-limits">
        {ordered.map((line) => (
          <li key={line.text} data-tone={line.warn ? 'warn' : 'neutral'}>
            {line.text}
          </li>
        ))}
      </ul>
      <p>{AT_REST}</p>
      {hardening !== null && <p data-tone="warn">{hardening}</p>}
      {holdsKey && <p>{APP_HOLDS_KEY}</p>}
    </div>
  );
}
