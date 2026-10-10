// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Windows only: the window has no decorations there, so the page draws minimize, maximize /
// restore and close, 46×38, in the system's own glyphs (Segoe Fluent Icons on Windows 11,
// Segoe MDL2 Assets on Windows 10). Close quits through the app, which waits for running
// operations; the window's own close is not granted (capabilities/main.json5).
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useEffect, useState } from 'react';
import { quit } from '../ipc/api';

const GLYPHS = {
  minimize: '',
  maximize: '',
  restore: '',
  close: '',
} as const;

const report = (what: string) => (error: unknown) => {
  console.error(`${what} failed:`, error);
};

export function CaptionButtons() {
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    const window = getCurrentWindow();
    let mounted = true;
    const check = () => {
      window
        .isMaximized()
        .then((now) => {
          if (mounted) setMaximized(now);
        })
        .catch(report('is_maximized'));
    };
    check();
    const unlisten = window.onResized(check);
    return () => {
      mounted = false;
      unlisten.then((off) => off()).catch(report('unlisten'));
    };
  }, []);

  const label = maximized ? 'Restore' : 'Maximize';
  return (
    <div className="captions">
      <button
        type="button"
        className="caption"
        aria-label="Minimize"
        title="Minimize"
        onClick={() => {
          getCurrentWindow().minimize().catch(report('minimize'));
        }}
      >
        <span aria-hidden="true">{GLYPHS.minimize}</span>
      </button>
      <button
        type="button"
        className="caption"
        aria-label={label}
        title={label}
        onClick={() => {
          getCurrentWindow().toggleMaximize().catch(report('toggle_maximize'));
        }}
      >
        <span aria-hidden="true">{maximized ? GLYPHS.restore : GLYPHS.maximize}</span>
      </button>
      <button
        type="button"
        className="caption"
        data-close
        aria-label="Close"
        title="Close"
        onClick={() => {
          quit().catch(report('quit'));
        }}
      >
        <span aria-hidden="true">{GLYPHS.close}</span>
      </button>
    </div>
  );
}
