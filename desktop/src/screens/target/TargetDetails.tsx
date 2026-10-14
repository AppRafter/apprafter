// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Target screen's main card: what `target show` prints, row by row, with the CLI's
// "not set", and the actions that change one field. The token shows as set and its length,
// never its value.
import type { ReactNode } from 'react';
import { Button } from '../../components/Button';
import { Card } from '../../components/Card';
import { CardRow } from '../../components/SettingRow';
import { Tag } from '../../components/Tag';
import type { Os } from '../../ipc/generated/Os';
import type { SecretsMode } from '../../ipc/generated/SecretsMode';
import type { TargetReport } from '../../ipc/generated/TargetReport';
import type { TokenPresence } from '../../ipc/generated/TokenPresence';
import type { StoreProblem } from '../../state/lockText';
import { secretCopy } from '../../state/secretCopy';
import { providerLabel, tierLabel } from '../targets/labels';
import { MachineRow } from './MachineRow';
import { keyValue } from './sshKey';

export interface TargetDetailActions {
  rename: () => void;
  renew: () => void;
  makeDefault: () => void;
  /** The SSH key row's Change: another key, alone (a key-only renewal; the token is kept). */
  changeSshKey: () => void;
}

export interface TargetDetailsProps {
  report: TargetReport;
  os: Os;
  secretsMode: SecretsMode;
  /**
   * What is wrong with the store, if anything (`AppInfo.secrets.problem`, D.4a critic B finding 8;
   * WI-476: other accounts can change it), as `storeProblemOf` tells it.
   */
  secretsProblem?: StoreProblem | null;
  actions: TargetDetailActions;
  /** Opens the machine picker (D.3e); null: no Change button. */
  onChangeMachine: (() => void) | null;
}

/** A row named for a screen reader by its label. */
function Group({ label, children }: { label: string; children: ReactNode }) {
  return (
    // biome-ignore lint/a11y/useSemanticElements: a row of read-only values, not form controls; a fieldset would announce a form
    <div role="group" aria-label={label}>
      {children}
    </div>
  );
}

const tokenValue = (token: TokenPresence) => {
  if (!token.set) return 'not set';
  return token.chars === null ? 'set' : `set · ${token.chars} characters`;
};

/** A value the card may cut short (a path, a key): its whole text shows on hover. */
const titled = (text: string) => <span title={text}>{text}</span>;

export function TargetDetails({
  report,
  os,
  secretsMode,
  secretsProblem = null,
  actions,
  onChangeMachine,
}: TargetDetailsProps) {
  return (
    <Card title="Target">
      <Group label="Name">
        <CardRow
          label="Name"
          value={report.name}
          control={
            <>
              {report.isCliDefault && (
                <Tag variant="outline" mono>
                  CLI default
                </Tag>
              )}
              <Button size={26} onClick={actions.rename}>
                Rename
              </Button>
            </>
          }
        />
      </Group>
      <Group label="CLI default">
        <CardRow
          label="CLI default"
          sub="What the apprafter CLI uses when no target is named."
          value={report.isCliDefault ? 'Yes' : 'No'}
          {...(!report.isCliDefault && {
            control: (
              <Button size={26} onClick={actions.makeDefault}>
                Make default
              </Button>
            ),
          })}
        />
      </Group>
      <Group label="Provider">
        <CardRow label="Provider" sub={report.provider} value={providerLabel(report.provider)} />
      </Group>
      <Group label="Region">
        <CardRow label="Region" value={report.region ?? 'not set'} />
      </Group>
      <Group label="Default tier">
        <CardRow label="Default tier" value={tierLabel(report.defaultTier, report.tierLevel)} />
      </Group>
      <Group label="Cluster name">
        <CardRow label="Cluster name" value={report.clusterName ?? 'not set'} />
      </Group>
      <Group label="Machine">
        <MachineRow report={report} onChange={onChangeMachine} />
      </Group>
      <Group label="API token">
        <CardRow
          label="API token"
          value={tokenValue(report.token)}
          {...(report.token.set && { sub: secretCopy(os, secretsMode, secretsProblem) })}
          control={
            <Button size={26} onClick={actions.renew}>
              Renew
            </Button>
          }
        />
      </Group>
      <Group label="SSH key">
        <CardRow
          label="SSH key"
          value={titled(keyValue(report.sshKey))}
          control={
            <Button size={26} onClick={actions.changeSshKey}>
              Change
            </Button>
          }
        />
      </Group>
      <Group label="Config file">
        <CardRow label="Config file" value={titled(report.configFile)} />
      </Group>
      <Group label="Credentials file">
        <CardRow label="Credentials file" value={titled(report.credentialsFile)} />
      </Group>
    </Card>
  );
}
