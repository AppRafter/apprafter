// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The toolchain (spec §5.1: tool_not_found / cue_not_found lead here; Settings › About opens it):
// each tool the app runs, what the probe found and where, and for one that is missing or broken
// the install lines for this OS. The probe is a plain command, each tool bounded by the core's
// TOOL_PROBE_TIMEOUT (R14). An install page is shown as its address: the opener's capability
// allows only the app's own three URLs.
import { useQuery } from '@tanstack/react-query';
import { Button } from '../../components/Button';
import { ErrorPanel } from '../../components/ErrorPanel';
import { ArrowsClockwiseIcon, SpinnerGapIcon } from '../../components/icons';
import { Modal } from '../../components/Modal';
import { StatePanel } from '../../components/StatePanel';
import { Tag } from '../../components/Tag';
import * as api from '../../ipc/api';
import { uiErrorOf } from '../../ipc/api';
import type { InstallHint } from '../../ipc/generated/InstallHint';
import type { ToolStatus } from '../../ipc/generated/ToolStatus';
import { usePlatform } from '../../state/platform';
import { HINT_LABELS, hintKind, hintsFor, pathSourceLine, toolStateLine } from './toolchain';

/** One install line: its OS, then the command or address in mono, or a note to read. */
function HintLine({ hint }: { hint: InstallHint }) {
  const kind = hintKind(hint.command);
  return (
    <div className="tool-hint" data-kind={kind}>
      <span className="tool-hint-os">{HINT_LABELS[hint.os]}</span>
      {kind === 'note' ? <span>{hint.command}</span> : <code>{hint.command}</code>}
    </div>
  );
}

function ToolRow({ status }: { status: ToolStatus }) {
  const { os } = usePlatform();
  const line = toolStateLine(status);
  return (
    <li className="tool-row">
      <div className="tool-head">
        <span className="tool-name">{status.tool}</span>
        <Tag variant="outline">{status.required ? 'required' : 'optional'}</Tag>
        <span className="tool-purpose">{status.purpose}</span>
      </div>
      <div className="tool-state" data-tone={line.tone}>
        {line.text}
      </div>
      {line.detail !== null && <div className="tool-detail">{line.detail}</div>}
      {status.path !== null && <div className="tool-path">{status.path}</div>}
      {status.problem !== null && (
        <div className="tool-hints">
          {hintsFor(status.install, os).map((hint) => (
            <HintLine key={`${hint.os}-${hint.command}`} hint={hint} />
          ))}
        </div>
      )}
    </li>
  );
}

export function ToolchainPanel({ onClose }: { onClose: () => void }) {
  const query = useQuery({ queryKey: ['toolchain'], queryFn: api.toolchainStatus });
  const report = query.data;
  return (
    <Modal
      title="Toolchain"
      sub="The tools AppRafter runs on this computer, and how to install one that is missing."
      width={620}
      layer="form"
      onClose={onClose}
      footer={
        <>
          <span className="tool-foot">
            {report !== undefined && (
              <details>
                <summary>
                  {pathSourceLine(report.searchPathSource, report.searchPath.length)}
                </summary>
                <ul className="tool-search-path">
                  {report.searchPath.map((dir) => (
                    <li key={dir}>{dir}</li>
                  ))}
                </ul>
              </details>
            )}
          </span>
          <Button
            icon={ArrowsClockwiseIcon}
            pending={query.isFetching}
            onClick={() => {
              void query.refetch();
            }}
          >
            Check again
          </Button>
        </>
      }
    >
      {query.isPending ? (
        <StatePanel icon={SpinnerGapIcon} spin title="Checking the tools…" />
      ) : query.isError ? (
        <ErrorPanel error={uiErrorOf(query.error)} />
      ) : (
        <ul className="tool-rows">
          {query.data.tools.map((status) => (
            <ToolRow key={status.tool} status={status} />
          ))}
        </ul>
      )}
    </Modal>
  );
}
