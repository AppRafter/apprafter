// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Settings (spec §4.6, brief §3), with the rows D.2 and D.4a have something behind: the theme; Lock
// — how the app locks, and a row that leads to the Security page, where the target store's own
// protection is (WI-476; LockSettings); the versions and links. Live data, notifications and the
// tray come with D.5, so those rows are not shown. The toolchain opens beside Settings, never in
// its panel: its layer then covers the window and makes Settings inert, and Esc closes it only.
// Opening Settings reads app_info again: a session watch that answered late, the authentication
// Rust found since, a store locked or put back in files from a terminal.
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
  MoonIcon,
  SunIcon,
  WrenchIcon,
} from '../components/icons';
import { Modal } from '../components/Modal';
import { SegmentedControl, type SegmentOption } from '../components/SegmentedControl';
import { SettingRow } from '../components/SettingRow';
import { uiErrorOf } from '../ipc/api';
import type { Settings } from '../ipc/generated/Settings';
import type { Theme } from '../ipc/generated/Theme';
import { ToolchainPanel } from '../screens/toolchain/ToolchainPanel';
import { osName, rereadAppInfo, usePlatform } from '../state/platform';
import { useSaveSettings, useSettings } from '../state/settings';
import { LINKS, openLink } from './links';
import { LockSettings } from './settings/LockSettings';
import { ThisComputerRow } from './ThisComputer';

const THEMES = [
  { value: 'system', label: 'System', icon: DesktopIcon },
  { value: 'light', label: 'Light', icon: SunIcon },
  { value: 'dark', label: 'Dark', icon: MoonIcon },
] as const satisfies readonly SegmentOption<Theme>[];

export interface SettingsDialogProps {
  readonly onClose: () => void;
  /** Settings' Security row: the shell closes Settings and shows the Security page (WI-476). */
  readonly onSecurity: () => void;
}

export function SettingsDialog({ onClose, onSecurity }: SettingsDialogProps) {
  const settings = useSettings();
  const client = useQueryClient();
  useEffect(() => rereadAppInfo(client, false), [client]);
  const [tools, setTools] = useState(false);
  return (
    <>
      <Modal title="Settings" width={620} onClose={onClose} dismissOnBackdrop>
        {settings.isPending ? null : settings.isError ? (
          <ErrorPanel error={uiErrorOf(settings.error)} />
        ) : (
          <SettingsBody
            settings={settings.data}
            onToolchain={() => setTools(true)}
            onSecurity={onSecurity}
          />
        )}
      </Modal>
      {tools && <ToolchainPanel onClose={() => setTools(false)} />}
    </>
  );
}

function SettingsBody({
  settings,
  onToolchain,
  onSecurity,
}: {
  settings: Settings;
  onToolchain: () => void;
  onSecurity: () => void;
}) {
  const info = usePlatform();
  const save = useSaveSettings();

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

      <LockSettings settings={settings} onOpenSecurity={onSecurity} />

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
