// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The target store's protection (D.4a §3.14, T74, T81; ADR 0068 §8): (1) Protect secrets — where
// the store's secrets are, and each state's way on; (2) the unlock note and the terminals line;
// (3) the slots, read-only in D.4a; (4) this computer — every limit the core reports, in the app's
// words, warnings first, what the lock does not cover, and what the app's own process could not
// do to protect the key (rule 1: never mask). Presentational: a container reads lock_info and
// app_info and runs the flows. Moved out of Settings › Lock's section by WI-476.
import { Button } from '../../components/Button';
import { ErrorPanel } from '../../components/ErrorPanel';
import { SettingRow } from '../../components/SettingRow';
import type { HolderHardening } from '../../ipc/generated/HolderHardening';
import type { LockReport } from '../../ipc/generated/LockReport';
import type { Os } from '../../ipc/generated/Os';
import type { SecretsMode } from '../../ipc/generated/SecretsMode';
import type { UiError } from '../../ipc/generated/UiError';
import {
  ACTION_LABELS,
  APP_HOLDS_KEY,
  AT_REST,
  hardeningLine,
  LIMITS_TITLE,
  type LimitText,
  limitText,
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

/**
 * What lock_info answers (the IPC type `LockSectionInfo`, d4a-g Task 11): the report and the app's
 * own protections.
 */
export interface LockInfoView {
  readonly report: LockReport;
  readonly app: HolderHardening;
}

export interface StoreProtectionProps {
  readonly os: Os;
  /** Where the store keeps its secrets (AppInfo.secrets.mode). */
  readonly mode: SecretsMode;
  /** The app's unlock opens the store's vault (AppInfo.secrets.protected): the unlock note. */
  readonly protectedStore: boolean;
  /** lock_info's answer; `null` while it is read, or when `error` says why it is not there. */
  readonly info: LockInfoView | null;
  readonly error: UiError | null;
  /**
   * The store's lock state cannot be used (AppInfo.secrets.problem: an unreadable store.yaml, a
   * lost vault; critic B finding 8): said, with why, in place of the state line and its actions.
   */
  readonly problem: UiError | null;
  readonly onAction: (action: ProtectAction) => void;
}

export function StoreProtection(p: StoreProtectionProps) {
  return (
    <>
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
          {p.protectedStore && <p>{unlockNote(p.os)}</p>}
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
