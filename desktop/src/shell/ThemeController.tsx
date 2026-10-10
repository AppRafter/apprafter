// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The theme setting, applied: above the platform and lock gates, so the lock screen and an
// error screen have the chosen theme too (settings_get is answered while locked). Until the
// settings arrive the default dark shows, and the window stays hidden (shell/reveal.tsx); if
// they cannot be read, the default stays and the window shows.
import { useEffect } from 'react';
import { useSettings } from '../state/settings';
import { followTheme } from '../state/theme';
import { useThemeApplied } from './reveal';

export function ThemeController() {
  const settings = useSettings();
  const theme = settings.data?.theme;
  const failed = settings.isError;
  const applied = useThemeApplied();
  useEffect(() => {
    if (theme === undefined) return undefined;
    const stop = followTheme(theme);
    applied();
    return stop;
  }, [theme, applied]);
  useEffect(() => {
    if (failed) applied();
  }, [failed, applied]);
  return null;
}
