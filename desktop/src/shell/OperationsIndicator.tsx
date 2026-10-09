// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// "N running" in the sidebar footer while operations run (spec §3.3 asks for the list; the
// design has none): it opens a sheet with each running operation and its Cancel.
import { Button } from '../components/Button';
import { SpinnerGapIcon } from '../components/icons';
import { Modal } from '../components/Modal';
import { cancel, type OpView, useOperations } from '../ipc/operations';
import { useOverlay } from './ViewFrame';

const isRunning = (op: OpView) => op.summary?.state === 'running';

export function OperationsIndicator() {
  const operations = useOperations();
  const show = useOverlay();
  const count = [...operations.values()].filter(isRunning).length;
  if (count === 0) return null;
  return (
    <button
      type="button"
      className="footer-item"
      onClick={() => show((close) => <RunningSheet onClose={close} />)}
    >
      <SpinnerGapIcon className="spin" aria-hidden="true" />
      <span className="nav-label">{`${count} running`}</span>
    </button>
  );
}

function RunningSheet({ onClose }: { onClose: () => void }) {
  const running = [...useOperations().values()].filter(isRunning);
  return (
    <Modal title="Running operations" width={540} onClose={onClose} dismissOnBackdrop>
      {running.length === 0 ? (
        <p className="sheet-empty">Nothing is running.</p>
      ) : (
        <ul className="running-list">
          {running.map((op) => {
            const title = op.summary?.title ?? `Operation ${op.opId}`;
            return (
              <li key={op.opId} className="running-item">
                <div className="running-text">
                  <div className="running-title">{title}</div>
                  {op.summary?.target != null && (
                    <div className="running-target">{op.summary.target}</div>
                  )}
                </div>
                <Button
                  size={26}
                  aria-label={`Cancel ${title}`}
                  onClick={() => {
                    cancel(op.opId).catch((error: unknown) =>
                      console.error(`op_cancel for ${op.opId} failed:`, error),
                    );
                  }}
                >
                  Cancel
                </Button>
              </li>
            );
          })}
        </ul>
      )}
    </Modal>
  );
}
