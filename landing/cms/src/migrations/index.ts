// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import * as migration_20260603_225750_initial from './20260603_225750_initial';
import * as migration_20260616_004517_honesty_waitlist_interests from './20260616_004517_honesty_waitlist_interests';

export const migrations = [
  {
    up: migration_20260603_225750_initial.up,
    down: migration_20260603_225750_initial.down,
    name: '20260603_225750_initial',
  },
  {
    up: migration_20260616_004517_honesty_waitlist_interests.up,
    down: migration_20260616_004517_honesty_waitlist_interests.down,
    name: '20260616_004517_honesty_waitlist_interests'
  },
];
