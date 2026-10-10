// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The one way to open the D.3 flows from a view. The add-target wizard, Doctor and the toolchain
// are the app's own overlays (shell/AppOverlays.tsx): over every view, the window around them
// inert. Change machine is an overlay of the view it is opened from (it hides with its tab, and
// its tab holds its confirm's plan). ErrorPanel actions these flows own are run here too; the
// others are their caller's.
import { useMemo, useState } from 'react';
import type { ErrorAction } from '../ipc/errors';
import type { UiError } from '../ipc/generated/UiError';
import { useAppOverlay } from '../shell/AppOverlays';
import { useOverlay } from '../shell/ViewFrame';
import { DoctorOverlay } from './doctor/DoctorOverlay';
import { ChangeMachineDialog, type MachineNow } from './machine/ChangeMachineDialog';
import { useChangeSshKey } from './target/actions';
import { ToolchainPanel } from './toolchain/ToolchainPanel';
import { AddTargetWizard } from './wizard/AddTargetWizard';

export interface TargetFlows {
  readonly addTarget: () => void;
  readonly doctor: (target: string) => void;
  readonly changeMachine: (target: string, now: MachineNow) => void;
  readonly toolchain: () => void;
  /** Runs `action` when these flows own it; whether it did. */
  readonly errorAction: (action: ErrorAction) => boolean;
}

interface DoctorFlowProps {
  readonly target: string;
  readonly onClose: () => void;
  /** Opens the wizard at the app's level: the doctor closes first, and the wizard stays. */
  readonly onAddTarget: () => void;
}

/**
 * Doctor with the Target screen's SSH key change bound to its three key fixes (none set, file
 * gone, not a public key): the form opens as an app overlay above the doctor, which stays for
 * Run again, and a refusal it cannot show itself is shown in the doctor. The form, its confirm
 * and the toolchain a missing tool's fix opens are the doctor's own overlays: they go with it.
 */
function DoctorFlow({ target, onClose, onAddTarget }: DoctorFlowProps) {
  const [keyFailure, setKeyFailure] = useState<UiError | null>(null);
  const changeSshKey = useChangeSshKey(target, setKeyFailure);
  const here = useAppOverlay();
  return (
    <DoctorOverlay
      target={target}
      onClose={onClose}
      onAddTarget={onAddTarget}
      onToolchain={() => here((close) => <ToolchainPanel onClose={close} />)}
      onChangeSshKey={() => {
        setKeyFailure(null);
        void changeSshKey();
      }}
      fixFailure={keyFailure}
    />
  );
}

export function useTargetFlows(): TargetFlows {
  const app = useAppOverlay();
  const view = useOverlay();
  return useMemo(() => {
    const toolchain = () => app((close) => <ToolchainPanel onClose={close} />);
    const addTarget = () => app((close) => <AddTargetWizard onClose={close} />);
    return {
      addTarget,
      toolchain,
      doctor: (target) =>
        app((close) => <DoctorFlow target={target} onClose={close} onAddTarget={addTarget} />),
      changeMachine: (target, now) =>
        view((close) => <ChangeMachineDialog target={target} now={now} onClose={close} />),
      errorAction: (action) => {
        if (action.kind === 'toolchain') {
          toolchain();
          return true;
        }
        if (action.kind === 'add-target') {
          addTarget();
          return true;
        }
        return false;
      },
    };
  }, [app, view]);
}
