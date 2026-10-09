// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The machine a target is set to, and the one its server runs on. A provisioned machine cannot
// change here: the row gives the CLI's rebuild recipe (render::core_error::resize_recipe) in
// place of Change. An unreadable local state says why and offers nothing either.
import { Button } from '../../components/Button';
import { CardRow } from '../../components/SettingRow';
import type { TargetReport } from '../../ipc/generated/TargetReport';

export interface MachineRowProps {
  report: TargetReport;
  /** Opens the machine picker (D.3e); null: no Change button. */
  onChange: (() => void) | null;
}

export function MachineRow({ report, onChange }: MachineRowProps) {
  const state = report.provisioned;
  const configured = report.serverType ?? 'not set';
  const running = state.status === 'provisioned' ? state.server.serverType : null;
  const value =
    running !== null && running !== report.serverType
      ? `${configured} · running ${running}`
      : configured;
  return (
    <CardRow
      label="Machine"
      value={value}
      {...(state.status === 'provisioned' && {
        sub: (
          <span className="target-row-sub">
            Provisioned as {state.server.serverName} (id {state.server.serverId}). Its machine
            cannot change in place: back it up and rebuild it on a new machine, in a terminal:
            <span className="target-recipe">
              <code>{`apprafter target use ${report.name}`}</code>
              <code>{'apprafter backup create --repo <repo>'}</code>
              <code>{'apprafter destroy --yes'}</code>
              <code>{'apprafter restore <repo> --reprovision --server-type <type>'}</code>
            </span>
            <code>destroy</code> deletes every resource labelled <code>apprafter=true</code> in the
            token's Hetzner project, not only this cluster: read “Moving to a bigger machine” in the
            operator guide first.
          </span>
        ),
      })}
      {...(state.status === 'unreadable' && {
        sub: `The local state cannot be read: ${state.error.message}`,
      })}
      {...(state.status === 'not_provisioned' &&
        onChange !== null && {
          control: (
            <Button size={26} onClick={onChange}>
              Change
            </Button>
          ),
        })}
    />
  );
}
