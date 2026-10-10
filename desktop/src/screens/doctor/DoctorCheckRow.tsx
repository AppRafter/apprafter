// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One doctor row (the design's): a tinted status badge, the core's title, its detail (a skipped
// row's detail is why it did not run), and the fix line, with the screen that fixes it when the
// app has one: a missing tool the toolchain, a missing target the add-target wizard, a target with
// no SSH key, or a key file that is gone, the key change.
import { Button } from '../../components/Button';
import { ArrowRightIcon } from '../../components/icons';
import { Tag } from '../../components/Tag';
import type { Check } from '../../ipc/generated/Check';
import type { CheckFix } from '../../ipc/generated/CheckFix';
import { fixText, inlineParts, STATUS_LABELS, STATUS_TONES } from './doctorText';

/** Text whose backticks are code spans. */
export function Inline({ text }: { text: string }) {
  return (
    <>
      {inlineParts(text).map((part, index) =>
        // The parts of one string, split once: the index is a stable key.
        'code' in part ? (
          // biome-ignore lint/suspicious/noArrayIndexKey: see above
          <code key={index}>{part.code}</code>
        ) : (
          // biome-ignore lint/suspicious/noArrayIndexKey: see above
          <span key={index}>{part.text}</span>
        ),
      )}
    </>
  );
}

const ACTIONS: Partial<Record<CheckFix['kind'], string>> = {
  install_tool: 'Show the toolchain',
  add_target: 'Add a target',
  configure_ssh_key: 'Change SSH key',
  ssh_key_missing: 'Change SSH key',
};

export interface DoctorCheckRowProps {
  readonly check: Check;
  /** The report's target, which a refused kubeconfig's fix names. */
  readonly target: string | null;
  /** Present: a fix that has a screen offers a button to it. */
  readonly onAction?: (fix: CheckFix) => void;
}

export function DoctorCheckRow({ check, target, onAction }: DoctorCheckRowProps) {
  const fix = check.fix;
  const text = fixText(check, target);
  const label = fix === null ? undefined : ACTIONS[fix.kind];
  return (
    <li className="doctor-row">
      <Tag tone={STATUS_TONES[check.status]} mono>
        {STATUS_LABELS[check.status]}
      </Tag>
      <div className="doctor-main">
        <div className="doctor-title">
          <Inline text={check.title} />
        </div>
        {check.detail !== null && (
          <div className="doctor-detail">
            <Inline text={check.detail} />
          </div>
        )}
        {fix !== null && text !== null && (
          <div className="doctor-fix">
            <ArrowRightIcon aria-hidden="true" />
            <span className="doctor-fix-text">
              <Inline text={text} />
            </span>
            {label !== undefined && onAction !== undefined && (
              <Button size={26} onClick={() => onAction(fix)}>
                {label}
              </Button>
            )}
          </div>
        )}
      </div>
    </li>
  );
}
