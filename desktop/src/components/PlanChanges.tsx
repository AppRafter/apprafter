// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What a plan changes, one row per object: the action as a tag, the object, and the core's
// one-line detail. Shared by the Target screen's confirms (D.3d) and the wizard (D.3e).
import type { ChangeAction } from '../ipc/generated/ChangeAction';
import type { PlannedChange } from '../ipc/generated/PlannedChange';
import { Tag } from './Tag';
import type { Tone } from './tone';

// Records over ChangeAction: an action Rust adds is a type error here until it has a label.
const ACTION_LABEL: Record<ChangeAction, string> = {
  create: 'Create',
  update: 'Update',
  replace: 'Replace',
  keep: 'Keep',
  rename: 'Rename',
  delete: 'Delete',
  set_default: 'Set default',
  clear_default: 'Clear default',
};

const ACTION_TONE: Record<ChangeAction, Tone> = {
  create: 'ok',
  update: 'warn',
  replace: 'warn',
  keep: 'neutral',
  rename: 'warn',
  delete: 'err',
  set_default: 'warn',
  clear_default: 'warn',
};

/** The core's object kinds (overview §3.5); another kind shows as it is. */
const KIND_LABEL: Readonly<Record<string, string>> = {
  Target: 'Target',
  Credentials: 'Credentials',
  LocalState: 'Local state',
  CliDefault: 'CLI default',
};

export const kindLabel = (kind: string): string =>
  Object.hasOwn(KIND_LABEL, kind) ? (KIND_LABEL[kind] ?? kind) : kind;

export function PlanChanges({ changes }: { changes: readonly PlannedChange[] }) {
  if (changes.length === 0) return <p className="plan-changes-none">Nothing changes.</p>;
  return (
    <ul className="plan-changes">
      {changes.map((change) => (
        <li
          key={`${change.kind}/${change.object}/${change.action}/${change.detail ?? ''}`}
          className="plan-change"
        >
          <Tag tone={ACTION_TONE[change.action]}>{ACTION_LABEL[change.action]}</Tag>
          <span className="plan-change-object">
            {kindLabel(change.kind)} {change.object}
          </span>
          {change.detail !== null && <span className="plan-change-detail">{change.detail}</span>}
        </li>
      ))}
    </ul>
  );
}
