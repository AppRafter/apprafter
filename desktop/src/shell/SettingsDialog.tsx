// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Settings (spec §4.6, brief §3), with the rows D.2 has something behind: the theme; the lock
// and what depends on it; the versions and links. Live data, notifications and the tray come
// with D.5, so those rows are not shown. With the lock off, the rows below it are disabled, not
// only dimmed. With no system authentication the lock shows as it is in effect, off with its
// switch disabled whatever settings.json says: Rust locks only with both, and refuses switching
// it on. "Lock when the computer sleeps or locks" says what the OS tells the app
// (AppInfo.sessionEvents): a note naming the half that is missing when it tells one, and the
// limit when it tells neither. It stays a choice whenever the lock is in effect, even then: every
// source is still listened to (a `loginctl lock-session`, a screen saver started later), so the
// owner must be able to switch it off. Opening Settings reads app_info again, so a session watch
// that answered late counts.
import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useState } from 'react';
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
  WrenchIcon,
} from '../components/icons';
import { Modal } from '../components/Modal';
import { SegmentedControl, type SegmentOption } from '../components/SegmentedControl';
import { SettingRow } from '../components/SettingRow';
import { Switch } from '../components/Switch';
import { uiErrorOf } from '../ipc/api';
import type { AutoLock } from '../ipc/generated/AutoLock';
import type { SessionEvents } from '../ipc/generated/SessionEvents';
import type { Settings } from '../ipc/generated/Settings';
import type { Theme } from '../ipc/generated/Theme';
import { ToolchainPanel } from '../screens/toolchain/ToolchainPanel';
import { authPrompt } from '../state/auth';
import { lockOff, NO_AUTH_NOTICE, useLockActions } from '../state/lock';
import { osName, rereadAppInfo, usePlatform } from '../state/platform';
import { useSaveSettings, useSettings } from '../state/settings';
import { shortcutHint } from '../state/shortcuts';
import { LINKS, openLink } from './links';
import { ThisComputerRow } from './ThisComputer';

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

/** The lock-on-sleep row's note: what the OS tells the app of its session, and what it does not. */
function sessionNote({ lock, sleep }: SessionEvents): string {
  if (lock && sleep) return 'When the screen locks or the computer goes to sleep.';
  if (lock) return 'AppRafter is told when the screen locks, not when the computer sleeps.';
  if (sleep) return 'AppRafter is told when the computer sleeps, not when the screen locks.';
  return 'This computer does not tell AppRafter when it locks or sleeps.';
}

export function SettingsDialog({ onClose }: { onClose: () => void }) {
  const settings = useSettings();
  const client = useQueryClient();
  // What app_info says may have changed since it was read: a session watch that answered late,
  // the authentication Rust found since.
  useEffect(() => rereadAppInfo(client, false), [client]);
  const [tools, setTools] = useState(false);
  // The toolchain beside Settings, never in its panel: its layer then covers the window and
  // makes Settings inert, and Esc closes the toolchain only.
  return (
    <>
      <Modal title="Settings" width={620} onClose={onClose} dismissOnBackdrop>
        {settings.isPending ? null : settings.isError ? (
          <ErrorPanel error={uiErrorOf(settings.error)} />
        ) : (
          <SettingsBody settings={settings.data} onToolchain={() => setTools(true)} />
        )}
      </Modal>
      {tools && <ToolchainPanel onClose={() => setTools(false)} />}
    </>
  );
}

function SettingsBody({ settings, onToolchain }: { settings: Settings; onToolchain: () => void }) {
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
      <SettingRow
        label="Lock when the computer sleeps or locks"
        sub={sessionNote(info.sessionEvents)}
        disabled={off}
        control={
          <Switch
            label="Lock when the computer sleeps or locks"
            checked={settings.lockOnSleep}
            disabled={off}
            onChange={(lockOnSleep) => save({ lockOnSleep })}
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
      <ThisComputerRow />
      <SettingRow
        label="Toolchain"
        sub="kubectl, helm, restic, git, ssh and cue on this computer."
        control={
          <Button size={28} icon={WrenchIcon} onClick={onToolchain}>
            Show
          </Button>
        }
      />
    </div>
  );
}
