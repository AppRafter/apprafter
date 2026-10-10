// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The window is created hidden, so it never flashes white, and shows on `window_ready`. The page
// says so once two things have committed: its first real screen (the lock screen, the shell, or
// an error, each marking itself with <ScreenShown />; never the blank while the first answers
// are on their way) and the theme (applied, or the settings unreadable and the default kept) —
// so a light theme never shows a dark page first.
//
// The signal goes from a timer after that commit, not from requestAnimationFrame: WebKitGTK runs
// no animation frames in a hidden window, so a frame callback would wait for the very window it
// is meant to show, until Rust's fallback shows it anyway.
import { createContext, type ReactNode, useContext, useEffect, useMemo, useState } from 'react';
import { windowReady } from '../ipc/api';

interface Reveal {
  /** A real screen has committed. */
  screen(): void;
  /** The theme is applied (or the settings could not be read, and the default stays). */
  theme(): void;
}

const NOTHING: Reveal = { screen() {}, theme() {} };
const RevealContext = createContext<Reveal>(NOTHING);

export function RevealWindow({ children }: { children: ReactNode }) {
  const [screen, setScreen] = useState(false);
  const [theme, setTheme] = useState(false);
  const reveal = useMemo<Reveal>(
    () => ({ screen: () => setScreen(true), theme: () => setTheme(true) }),
    [],
  );
  const ready = screen && theme;
  useEffect(() => {
    if (!ready) return;
    // After the commit and a turn of the event loop, so the screen is painted when it shows.
    const handle = setTimeout(() => {
      windowReady().catch((error: unknown) => console.error('window_ready failed:', error));
    }, 0);
    return () => clearTimeout(handle);
  }, [ready]);
  return <RevealContext value={reveal}>{children}</RevealContext>;
}

/** Marks a real screen: rendered inside one, it lets the window show. */
export function ScreenShown(): null {
  const { screen } = useContext(RevealContext);
  useEffect(screen, [screen]);
  return null;
}

/** For the theme controller: call once the theme is applied, or known to stay the default. */
export function useThemeApplied(): () => void {
  return useContext(RevealContext).theme;
}
