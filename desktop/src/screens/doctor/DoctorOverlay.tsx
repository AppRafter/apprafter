// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Doctor (spec §7): groups Target / Cluster / This computer, run as a read operation for one
// target (the tab's), its stage shown live (core Event::Stage per group, 1-based) and cancellable.
// A fix that has a screen offers it: a missing tool the toolchain, a missing target the wizard
// (the doctor closes first: the wizard's layer is below the doctor's), a target with no SSH key
// the key change (the Target screen's, which reads the key in use itself; its form opens above
// the doctor, which stays for Run again).
import { useCallback, useEffect, useId, useState } from 'react';
import { Button } from '../../components/Button';
import { ErrorPanel } from '../../components/ErrorPanel';
import { IconButton } from '../../components/IconButton';
import {
  ArrowsClockwiseIcon,
  SpinnerGapIcon,
  StethoscopeIcon,
  XIcon,
} from '../../components/icons';
import { ModalFrame } from '../../components/Modal';
import { StatePanel } from '../../components/StatePanel';
import { Tag } from '../../components/Tag';
import * as api from '../../ipc/api';
import type { CheckFix } from '../../ipc/generated/CheckFix';
import type { DoctorReport } from '../../ipc/generated/DoctorReport';
import type { OpId } from '../../ipc/generated/OpId';
import { useOperation } from '../../ipc/operations';
import { useRead } from '../../state/read';
import { DoctorCheckRow } from './DoctorCheckRow';
import { type CheckCounts, checkCounts, GROUP_TITLES, stamp } from './doctorText';

export interface DoctorOverlayProps {
  readonly target: string;
  readonly onClose: () => void;
  readonly onAddTarget: () => void;
  readonly onToolchain: () => void;
  /** The `configure_ssh_key` fix: the Target screen's key change for this target. */
  readonly onChangeSshKey: () => void;
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
}: DoctorOverlayProps) {
  const id = useId();
  const read = useRead<DoctorReport>();
  const [at, setAt] = useState<Date | null>(null);
  const { run } = read;
  const start = useCallback(() => {
    void run(() => api.opStartDoctor(target)).then((report) => {
      if (report !== null) setAt(new Date());
    });
  }, [run, target]);
  useEffect(() => {
    start();
  }, [start]);

  const onAction = (fix: CheckFix) => {
    if (fix.kind === 'install_tool') onToolchain();
    else if (fix.kind === 'configure_ssh_key') onChangeSshKey();
    else if (fix.kind === 'add_target') {
      onClose();
      onAddTarget();
    }
  };

  const s = read.state;
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
        {s.status === 'running' && (
          <StatePanel
            icon={SpinnerGapIcon}
            spin
            title="Checking the target, its cluster and this computer…"
            text={s.opId === null ? undefined : <StageLine opId={s.opId} />}
            actions={<Button onClick={read.cancel}>Cancel</Button>}
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
          variant="primary"
          icon={ArrowsClockwiseIcon}
          disabled={s.status === 'running'}
          onClick={start}
        >
          Run again
        </Button>
      </div>
    </ModalFrame>
  );
}
