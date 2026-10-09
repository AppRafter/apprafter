// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Settings (spec §4.6, brief §3), with the rows D.2 has something behind: the theme; the lock
// and what depends on it; the versions and links. Live data, notifications and the tray come
// with D.5, and "lock when the computer sleeps" with D.2d's session sources (AppInfo does not
// say yet whether they exist), so those rows are not shown. With the lock off, the rows below
// it are disabled, not only dimmed. With no system authentication the lock shows as it is in
// effect, off with its switch disabled whatever settings.json says: Rust locks only with both,
// and refuses switching it on.
import { useQueryClient } from '@tanstack/react-query';
import { useEffect } from 'react';
import { Button } from '../components/Button';
import { ErrorPanel } from '../components/ErrorPanel';
import { Eyebrow } from '../components/Eyebrow';
import {
  BookOpenIcon,
  DesktopIcon,
  GithubLogoIcon,
  GlobeSimpleIcon,
  LockSimpleIcon,
  MoonIcon,
  SunIcon,
} from '../components/icons';
import { Modal } from '../components/Modal';
import { SegmentedControl, type SegmentOption } from '../components/SegmentedControl';
import { SettingRow } from '../components/SettingRow';
import { Switch } from '../components/Switch';
import { uiErrorOf } from '../ipc/api';
import type { AutoLock } from '../ipc/generated/AutoLock';
import type { Settings } from '../ipc/generated/Settings';
import type { Theme } from '../ipc/generated/Theme';
import { authPrompt } from '../state/auth';
import { lockOff, NO_AUTH_NOTICE, useLockActions } from '../state/lock';
import { osName, rereadAppInfo, usePlatform } from '../state/platform';
import { useSaveSettings, useSettings } from '../state/settings';
import { shortcutHint } from '../state/shortcuts';
import { LINKS, openLink } from './links';

const THEMES = [
  { value: 'system', label: 'System', icon: DesktopIcon },
  { value: 'light', label: 'Light', icon: SunIcon },
  { value: 'dark', label: 'Dark', icon: MoonIcon },
] as const satisfies readonly SegmentOption<Theme>[];

const AUTO_LOCK = [
  { value: '5', label: '5 min' },
  { value: '10', label: '10 min' },
  { value: '30', label: '30 min' },
  { value: 'never', label: 'Never' },
] as const satisfies readonly SegmentOption<AutoLock>[];

export function SettingsDialog({ onClose }: { onClose: () => void }) {
  const settings = useSettings();
  const client = useQueryClient();
  // What app_info says may have changed since it was read: a session watch that answered late,
  // the authentication Rust found since.
  useEffect(() => rereadAppInfo(client, false), [client]);
  return (
    <Modal title="Settings" width={620} onClose={onClose} dismissOnBackdrop>
      {settings.isPending ? null : settings.isError ? (
        <ErrorPanel error={uiErrorOf(settings.error)} />
      ) : (
        <SettingsBody settings={settings.data} />
      )}
    </Modal>
  );
}

function SettingsBody({ settings }: { settings: Settings }) {
  const info = usePlatform();
  const save = useSaveSettings();
  const { lock } = useLockActions();
  const noAuth = !info.auth.available;
  const off = lockOff(info.auth.available, settings.lockEnabled) !== null;
  const method = info.auth.method;

  return (
    <div className="settings">
      {info.settingsNotice !== null && (
        <p className="settings-banner" role="note">
          {info.settingsNotice}
        </p>
      )}

      <Eyebrow>Appearance</Eyebrow>
      <SettingRow
        label="Theme"
        sub={`System follows the ${osName(info.os)} appearance.`}
        control={
          <SegmentedControl
            ariaLabel="Theme"
            value={settings.theme}
            options={THEMES}
            onChange={(theme) => save({ theme })}
          />
        }
      />

      <Eyebrow>Security</Eyebrow>
      {noAuth && (
        <p className="settings-banner" role="note">
          {NO_AUTH_NOTICE}
        </p>
      )}
      <SettingRow
        label="Require unlock"
        disabled={noAuth}
        sub={
          method === null
            ? 'Ask for the computer’s owner before showing any cluster.'
            : `Ask for ${authPrompt(method)} before showing any cluster.`
        }
        control={
          <Switch
            label="Require unlock"
            checked={settings.lockEnabled && !noAuth}
            disabled={noAuth}
            onChange={(lockEnabled) => save({ lockEnabled })}
          />
        }
      />
      <SettingRow
        label="Lock when the app starts"
        sub="The app opens on the lock screen."
        disabled={off}
        control={
          <Switch
            label="Lock when the app starts"
            checked={settings.lockOnStart}
            disabled={off}
            onChange={(lockOnStart) => save({ lockOnStart })}
          />
        }
      />
      {info.auth.biometricsChoice && (
        <SettingRow
          label="Prefer biometrics"
          sub="Fingerprint, face or PIN before the password."
          disabled={off}
          control={
            <Switch
              label="Prefer biometrics"
              checked={settings.hello}
              disabled={off}
              onChange={(hello) => save({ hello })}
            />
          }
        />
      )}
      <SettingRow
        label="Auto-lock after inactivity"
        sub="How long the app waits before it locks itself."
        disabled={off}
        control={
          <SegmentedControl
            ariaLabel="Auto-lock after inactivity"
            font="mono"
            value={settings.autoLock}
            options={AUTO_LOCK}
            disabled={off}
            onChange={(autoLock) => save({ autoLock })}
          />
        }
      />
      <SettingRow
        label="Lock now"
        sub={`Also ${shortcutHint('lock', info.os)}.`}
        disabled={off}
        control={
          <Button
            size={28}
            icon={LockSimpleIcon}
            disabled={off}
            onClick={() => {
              lock().catch((error: unknown) => console.error('lock_now failed:', error));
            }}
          >
            Lock
          </Button>
        }
      />

      <Eyebrow>About</Eyebrow>
      <SettingRow
        label={`AppRafter Desktop ${info.desktopVersion} · core ${info.coreVersion}`}
        control={
          <>
            <Button size={28} icon={GlobeSimpleIcon} onClick={() => openLink(LINKS.website)}>
              Website
            </Button>
            <Button size={28} icon={BookOpenIcon} onClick={() => openLink(LINKS.docs)}>
              Docs
            </Button>
            <Button size={28} icon={GithubLogoIcon} onClick={() => openLink(LINKS.github)}>
              GitHub
            </Button>
          </>
        }
      />
    </div>
  );
}
