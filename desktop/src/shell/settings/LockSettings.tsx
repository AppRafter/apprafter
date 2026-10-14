// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Settings › Lock's container (D.4a §3.14; WI-476): the Lock section on what app_info and the
// settings say. It reads no lock_info: the store's report, its flows and this computer's limits
// are the Security page's (screens/security/SecurityPage.tsx), which `onOpenSecurity` shows.
import type { Settings } from '../../ipc/generated/Settings';
import { useLockActions } from '../../state/lock';
import { usePlatform } from '../../state/platform';
import { useSaveSettings } from '../../state/settings';
import { LockSection } from './LockSection';

export interface LockSettingsProps {
  readonly settings: Settings;
  /** Settings closes and the Security page shows (the Shell's). */
  readonly onOpenSecurity: () => void;
}

/** The Lock section, on app_info and the settings. */
export function LockSettings({ settings, onOpenSecurity }: LockSettingsProps) {
  const info = usePlatform();
  const save = useSaveSettings();
  const { lock } = useLockActions();
  return (
    <LockSection
      settings={settings}
      os={info.os}
      auth={info.auth}
      sessionEvents={info.sessionEvents}
      mode={info.secrets.mode}
      protectedStore={info.secrets.protected}
      storeProblem={info.secrets.problem !== null}
      onSave={(change) => save(change)}
      onLock={() => {
        lock().catch((error: unknown) => console.error('lock_now failed:', error));
      }}
      onOpenSecurity={onOpenSecurity}
    />
  );
}
