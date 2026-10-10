// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Adding a target is the D.3 wizard: shown so the page has its shape, disabled until then.
import { PlusCircleIcon } from '../../components/icons';

export function AddTargetCard() {
  return (
    <button type="button" className="add-target-card" disabled>
      <PlusCircleIcon aria-hidden="true" />
      <span className="add-target-title">Add target</span>
      <span className="add-target-sub">Hetzner Cloud token, region, machine</span>
      <span className="add-target-when">Arrives in D.3</span>
    </button>
  );
}
