// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The left column of a view (brief §2). On a target tab: the target's name, the sections and the
// Manage sections. Always: the footer — Targets, Settings, Lock with their shortcuts, the
// running operations, the three links and the version. On the Targets view only the footer.
import { openUrl } from '@tauri-apps/plugin-opener';
import { Dot } from '../components/Dot';
import { Eyebrow } from '../components/Eyebrow';
import { IconButton } from '../components/IconButton';
import {
  BookOpenIcon,
  GearSixIcon,
  GithubLogoIcon,
  GlobeSimpleIcon,
  HardDrivesIcon,
  type Icon,
  LockSimpleIcon,
} from '../components/icons';
import { Kbd } from '../components/Kbd';
import type { AppInfo } from '../ipc/generated/AppInfo';
import type { Os } from '../ipc/generated/Os';
import type { Section, TargetTab } from '../state/session';
import { type ShortcutAction, shortcutHint } from '../state/shortcuts';
import { OperationsIndicator } from './OperationsIndicator';
import { SECTIONS, type SectionInfo } from './sections';

/** The capability lets the opener open exactly these (capabilities/main.json5). */
export const LINKS = {
  website: 'https://apprafter.dev',
  docs: 'https://docs.apprafter.dev',
  github: 'https://github.com/AppRafter/apprafter',
} as const;

export interface SidebarProps {
  os: Os;
  /** The tab this sidebar belongs to; null on the Targets view (footer only). */
  tab: TargetTab | null;
  info: AppInfo;
  onNavigate: (section: Section) => void;
  onShowTargets: () => void;
  onLock: () => void;
  /** Opens Settings; without it there is no Settings entry. */
  onSettings?: () => void;
}

const open = (url: string) => () => {
  openUrl(url).catch((error: unknown) => console.error(`${url} did not open:`, error));
};

export function Sidebar({
  os,
  tab,
  info,
  onNavigate,
  onShowTargets,
  onLock,
  onSettings,
}: SidebarProps) {
  const nav = (group: SectionInfo['group']) =>
    SECTIONS.filter((s) => s.group === group).map((section) => (
      <NavItem
        key={section.id}
        section={section}
        current={tab?.section === section.id}
        onClick={() => onNavigate(section.id)}
      />
    ));
  return (
    <aside className="sidebar">
      {tab !== null && (
        <>
          <div className="cluster-header">
            <Dot size={8} />
            <span className="cluster-name">{tab.target}</span>
          </div>
          <nav className="nav" aria-label="Sections">
            {nav('main')}
          </nav>
          <Eyebrow>Manage</Eyebrow>
          <nav className="nav nav-manage" aria-label="Manage">
            {nav('manage')}
          </nav>
        </>
      )}
      <div className="sidebar-fill" />
      <div className="sidebar-footer">
        <FooterItem
          icon={HardDrivesIcon}
          label="Targets"
          os={os}
          shortcut="targets"
          onClick={onShowTargets}
        />
        {onSettings !== undefined && (
          <FooterItem
            icon={GearSixIcon}
            label="Settings"
            os={os}
            shortcut="settings"
            onClick={onSettings}
          />
        )}
        <FooterItem icon={LockSimpleIcon} label="Lock" os={os} shortcut="lock" onClick={onLock} />
        <OperationsIndicator />
        <div className="sidebar-links">
          <IconButton
            label="Website"
            icon={GlobeSimpleIcon}
            size={26}
            onClick={open(LINKS.website)}
          />
          <IconButton label="Docs" icon={BookOpenIcon} size={26} onClick={open(LINKS.docs)} />
          <IconButton label="GitHub" icon={GithubLogoIcon} size={26} onClick={open(LINKS.github)} />
          <span className="sidebar-version">{`v${info.desktopVersion}`}</span>
        </div>
      </div>
    </aside>
  );
}

function NavItem({
  section,
  current,
  onClick,
}: {
  section: SectionInfo;
  current: boolean;
  onClick: () => void;
}) {
  const SectionIcon = section.icon;
  return (
    <button
      type="button"
      className="nav-item"
      aria-current={current ? 'page' : undefined}
      onClick={onClick}
    >
      <SectionIcon aria-hidden="true" />
      <span className="nav-label">{section.label}</span>
    </button>
  );
}

function FooterItem({
  icon: ItemIcon,
  label,
  os,
  shortcut,
  onClick,
}: {
  icon: Icon;
  label: string;
  os: Os;
  shortcut: ShortcutAction;
  onClick: () => void;
}) {
  const hint = shortcutHint(shortcut, os);
  return (
    <button
      type="button"
      className="footer-item"
      aria-keyshortcuts={hint.replace('⌘', 'Meta+').replace('Ctrl', 'Control')}
      onClick={onClick}
    >
      <ItemIcon aria-hidden="true" />
      <span className="nav-label">{label}</span> <Kbd>{hint}</Kbd>
    </button>
  );
}
