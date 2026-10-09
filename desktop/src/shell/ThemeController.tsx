// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The theme setting, applied: above the lock gate, so the lock screen has the chosen theme too
// (settings_get is answered while locked). Until the settings arrive, the default dark shows.
import { useEffect } from 'react';
import { useSettings } from '../state/settings';
import { followTheme } from '../state/theme';

export function ThemeController() {
  const theme = useSettings().data?.theme;
  useEffect(() => (theme === undefined ? undefined : followTheme(theme)), [theme]);
  return null;
}
