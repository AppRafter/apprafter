// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Target screen's Danger zone: remove the target from this computer. Destroying the
// infrastructure arrives with its own slice (D.12), not as a disabled row here.
import { Button } from '../../components/Button';
import { Card } from '../../components/Card';
import { TrashIcon } from '../../components/icons';
import { CardRow } from '../../components/SettingRow';

export function DangerZone({ onRemove }: { onRemove: () => void }) {
  return (
    <Card title="Danger zone" tone="danger">
      <CardRow
        label="Remove from this computer"
        sub="Forgets its config, token and local state on this computer. Nothing changes at the provider; a provisioned server keeps running."
        control={
          <Button size={26} variant="danger" icon={TrashIcon} onClick={onRemove}>
            Remove…
          </Button>
        }
      />
    </Card>
  );
}
