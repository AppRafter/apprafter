// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The 38px bar (brief §2): logo, tabs (children), a spacer, and per OS the rest. Windows draws
// its caption buttons; macOS leaves the traffic lights their place at the left; Linux keeps its
// native title bar above this one, so nothing here drags the window. On Windows and macOS the
// bar, the logo zone and the spacer drag it (Tauri reads the attribute on the element pressed:
// `true` for that element only, `deep` for its whole subtree).
import type { ReactNode } from 'react';
import { Logo } from '../components/Logo';
import type { Os } from '../ipc/generated/Os';
import { CaptionButtons } from './CaptionButtons';

export function TitleBar({ os, children }: { os: Os; children: ReactNode }) {
  const drags = os !== 'linux';
  const region = (value: 'true' | 'deep') => (drags ? value : undefined);
  return (
    <header className="titlebar" data-os={os} data-tauri-drag-region={region('true')}>
      {os === 'macos' && <div className="traffic-inset" data-tauri-drag-region="true" />}
      <div className="logo-zone" data-tauri-drag-region={region('deep')}>
        <Logo />
      </div>
      {children}
      <div className="titlebar-spacer" data-tauri-drag-region={region('true')} />
      {os === 'windows' && <CaptionButtons />}
    </header>
  );
}
