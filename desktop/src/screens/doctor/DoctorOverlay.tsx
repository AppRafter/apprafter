// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Doctor (spec §7): groups Target / Cluster / This computer, run as a read operation for one
// target (the tab's), its stage shown live (core Event::Stage per group, 1-based) and cancellable.
// Copy report writes the plain-text report once there is one (the clipboard is write-only).
// A fix that has a screen offers it: a missing tool the toolchain, a missing target the wizard
// (the doctor closes first: the wizard's layer is below the doctor's), a target with no SSH key
// the key change (the Target screen's, which reads the key in use itself; its form opens above
// the doctor, which stays for Run again). What the core warns of shows as it arrives, while the
// run goes; a warning that never reached the screen, its doctor closed first, goes to the app's
// notices (review #6).
import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { Button } from '../../components/Button';
import { ErrorPanel } from '../../components/ErrorPanel';
import { IconButton } from '../../components/IconButton';
import {
  ArrowsClockwiseIcon,
  CopyIcon,
  SpinnerGapIcon,
  StethoscopeIcon,
  WarningCircleIcon,
  XIcon,
} from '../../components/icons';
import { ModalFrame } from '../../components/Modal';
import { StatePanel } from '../../components/StatePanel';
import { Tag } from '../../components/Tag';
import * as api from '../../ipc/api';
import { keepEndedAway } from '../../ipc/away';
import type { CheckFix } from '../../ipc/generated/CheckFix';
import type { DoctorReport } from '../../ipc/generated/DoctorReport';
import type { OpId } from '../../ipc/generated/OpId';
import type { UiError } from '../../ipc/generated/UiError';
import { useOperation } from '../../ipc/operations';
import { notesIn, type OpNote } from '../../ipc/plans';
import { useCopy } from '../../state/copy';
import { useRead } from '../../state/read';
import { useScope } from '../../state/scope';
import { DoctorCheckRow } from './DoctorCheckRow';
import { type CheckCounts, checkCounts, GROUP_TITLES, reportText, stamp } from './doctorText';

export interface DoctorOverlayProps {
  readonly target: string;
  readonly onClose: () => void;
  readonly onAddTarget: () => void;
  readonly onToolchain: () => void;
  /** The SSH key fixes (none set, file gone, not a public key): the Target screen's key change. */
  readonly onChangeSshKey: () => void;
  /** A fix's flow that failed where it could show nothing itself (the key change's reads). */
  readonly fixFailure?: UiError | null;
}

/** The header's chips: one per status that has rows, in the order a reader triages them. */
function SummaryChips({ counts }: { counts: CheckCounts }) {
  const chips = [
    { n: counts.pass, label: 'pass', tone: 'ok' },
    { n: counts.warn, label: 'warn', tone: 'warn' },
    { n: counts.fail, label: 'fail', tone: 'err' },
    { n: counts.skipped, label: 'skipped', tone: 'neutral' },
  ] as const;
  return (
    <span className="doctor-chips">
      {chips
        .filter((chip) => chip.n > 0)
        .map((chip) => (
          <Tag key={chip.label} tone={chip.tone} mono>
            {`${chip.n} ${chip.label}`}
          </Tag>
        ))}
    </span>
  );
}

/** `kept`, then each of `more` whose text is not among them yet: every note once. */
function withNotes(kept: readonly OpNote[], more: readonly OpNote[]): readonly OpNote[] {
  const all = [...kept];
  for (const note of more) if (!all.some((k) => k.text === note.text)) all.push(note);
  return all;
}

/** No operation has this id: what the live notes follow while no run goes. */
const NO_RUN: OpId = -1;

/** The running stage, from the operations store that follows the run: "Cluster · 2 of 3". */
function StageLine({ opId }: { opId: OpId }) {
  const stage = useOperation(opId)?.stage ?? null;
  return stage === null ? null : `${stage.title} · ${stage.index} of ${stage.total}`;
}

export function DoctorOverlay({
  target,
  onClose,
  onAddTarget,
  onToolchain,
  onChangeSshKey,
  fixFailure = null,
}: DoctorOverlayProps) {
  const id = useId();
  const copy = useCopy();
  const scope = useScope();
  const read = useRead<DoctorReport>();
  const [at, setAt] = useState<Date | null>(null);
  // What the core warned of in an ended run (a failed sweep of decrypted kubeconfig copies):
  // kept, each once, until the overlay closes (review #5).
  const [notes, setNotes] = useState<readonly OpNote[]>([]);
  // The texts this overlay has put on the screen: a note it never showed goes to the app.
  const shown = useRef(new Set<string>());
  const { run } = read;
  const start = useCallback(() => {
    void run(
      () => api.opStartDoctor(target),
      undefined,
      (more) => {
        // The overlay went before the run ended (closed, or the lock): a warning it never
        // showed — the sweep's comes before any check — goes to the app's notices, not lost
        // with the operation. A notice only informs, and goes with the overlay.
        if (scope.gone()) {
          for (const note of more) {
            if (note.kind !== 'warning' || shown.current.has(note.text)) continue;
            keepEndedAway({ opId: null, text: `Doctor · ${target}: ${note.text}`, failed: true });
          }
          return;
        }
        setNotes((kept) => withNotes(kept, more));
      },
    ).then((report) => {
      if (report !== null) setAt(new Date());
    });
  }, [run, target, scope]);
  useEffect(() => {
    start();
  }, [start]);

  const onAction = (fix: CheckFix) => {
    if (fix.kind === 'install_tool') onToolchain();
    else if (
      fix.kind === 'configure_ssh_key' ||
      fix.kind === 'ssh_key_missing' ||
      fix.kind === 'ssh_key_not_public'
    ) {
      onChangeSshKey();
    } else if (fix.kind === 'add_target') {
      onClose();
      onAddTarget();
    }
  };

  const s = read.state;
  // The run's warnings as they arrive (the core sends the sweep's before any check), beside
  // those kept from ended runs: on the screen while the run goes, not only once it ends.
  const live = useOperation(s.status === 'running' ? (s.opId ?? NO_RUN) : NO_RUN);
  const onScreen = withNotes(notes, notesIn(live?.lines ?? []));
  useEffect(() => {
    for (const note of onScreen) shown.current.add(note.text);
  });
  // Cancel goes with the run: had it the focus, the focus goes to Run again (review #7).
  const cancelFocused = useRef(false);
  const runAgainRef = useRef<HTMLButtonElement>(null);
  const running = s.status === 'running';
  useLayoutEffect(() => {
    if (running || !cancelFocused.current) return;
    cancelFocused.current = false;
    runAgainRef.current?.focus();
  }, [running]);
  const report = s.status === 'done' ? s.data : null;
  const counts = report === null ? null : checkCounts(report);
  return (
    <ModalFrame
      labelledBy={`${id}-title`}
      width={660}
      layer="doctor"
      onClose={onClose}
      dismissOnBackdrop
    >
      <div className="modal-head">
        <StethoscopeIcon className="modal-icon doctor-icon" aria-hidden="true" />
        <h2 className="modal-title doctor-heading" id={`${id}-title`}>
          {`Doctor · ${target}`}
        </h2>
        {counts !== null && <SummaryChips counts={counts} />}
        <IconButton label="Close" icon={XIcon} onClick={onClose} />
      </div>
      <div className="modal-body doctor-body" data-modal-body>
        {fixFailure !== null && <ErrorPanel error={fixFailure} />}
        {onScreen.length > 0 && (
          <ul className="doctor-notes" aria-label="Warnings">
            {onScreen.map((note) => (
              <li key={note.text} data-kind={note.kind}>
                <WarningCircleIcon aria-hidden="true" />
                <span>{note.text}</span>
              </li>
            ))}
          </ul>
        )}
        {s.status === 'running' && (
          <StatePanel
            icon={SpinnerGapIcon}
            spin
            title="Checking the target, its cluster and this computer…"
            text={s.opId === null ? undefined : <StageLine opId={s.opId} />}
            actions={
              <Button
                onClick={read.cancel}
                onFocus={() => {
                  cancelFocused.current = true;
                }}
                onBlur={() => {
                  cancelFocused.current = false;
                }}
              >
                Cancel
              </Button>
            }
          />
        )}
        {s.status === 'failed' && <ErrorPanel error={s.error} />}
        {s.status === 'cancelled' && <p className="doctor-note">Doctor was cancelled.</p>}
        {report?.groups.map((group) => (
          <section key={group.id} className="doctor-group" aria-labelledby={`${id}-${group.id}`}>
            <h3 className="eyebrow" id={`${id}-${group.id}`}>
              {GROUP_TITLES[group.id]}
            </h3>
            <ul className="doctor-rows">
              {group.checks.map((check, index) => (
                <DoctorCheckRow
                  // A run's check list is fixed; id + tool + index is stable within it.
                  // biome-ignore lint/suspicious/noArrayIndexKey: see above
                  key={`${check.id}-${check.tool ?? ''}-${index}`}
                  check={check}
                  target={report.target}
                  onAction={onAction}
                />
              ))}
            </ul>
          </section>
        ))}
      </div>
      <div className="modal-foot">
        <span className="doctor-foot">
          {counts !== null && at !== null
            ? `${counts.total} ${counts.total === 1 ? 'check' : 'checks'} · ${stamp(at).slice(11)}`
            : ''}
        </span>
        <Button
          icon={CopyIcon}
          disabled={report === null || at === null}
          onClick={() => {
            if (report !== null && at !== null) {
              copy(reportText(report, at, notes), 'Report copied');
            }
          }}
        >
          Copy report
        </Button>
        <Button
          variant="primary"
          icon={ArrowsClockwiseIcon}
          ref={runAgainRef}
          pending={running}
          onClick={start}
        >
          Run again
        </Button>
      </div>
    </ModalFrame>
  );
}
