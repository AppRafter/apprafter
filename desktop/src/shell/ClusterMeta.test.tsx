// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { clearMocks } from '@tauri-apps/api/mocks';
import { screen } from '@testing-library/react';
import * as api from '../ipc/api';
import { installMockIpc } from '../ipc/mock';
import { renderScreen } from '../test/screens';
import { ClusterMeta } from './ClusterMeta';

beforeEach(async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
});
afterEach(() => clearMocks());

test("a target's provider · region · tier, from the store's list", async () => {
  renderScreen(<ClusterMeta target="prod-eu" />);
  expect((await screen.findByText('hetzner-cloud · nbg1 · T2')).className).toBe('cluster-meta');
});

test('what is not set is left out', async () => {
  renderScreen(<ClusterMeta target="lab" />);
  expect(await screen.findByText('hetzner-cloud · hel1')).toBeDefined();
});

test('nothing for a name the list does not hold', async () => {
  renderScreen(
    <>
      <ClusterMeta target="ghost" />
      <ClusterMeta target="staging" />
    </>,
  );
  await screen.findByText('hetzner-cloud · fsn1 · T1');
  expect(document.querySelectorAll('.cluster-meta')).toHaveLength(1);
});
