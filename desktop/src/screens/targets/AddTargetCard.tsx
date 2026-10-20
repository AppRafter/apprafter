// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Adding a target: the card opens the add-target wizard, an app overlay over every view.
import { PlusCircleIcon } from '../../components/icons';
import { useTargetFlows } from '../flows';

export function AddTargetCard() {
  const flows = useTargetFlows();
  return (
    <button type="button" className="add-target-card" onClick={flows.addTarget}>
      <PlusCircleIcon aria-hidden="true" />
      <span className="add-target-title">Add target</span>
      <span className="add-target-sub">Hetzner Cloud token, region, machine</span>
    </button>
  );
}
