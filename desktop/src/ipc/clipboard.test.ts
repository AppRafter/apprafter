// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, expect, test } from 'bun:test';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { settleIpc } from '../test/settle';
import { copyText } from './clipboard';

afterEach(async () => {
  await settleIpc();
  clearMocks();
});

test('copyText writes the text through the clipboard plugin, nothing else', async () => {
  const calls: { cmd: string; args: unknown }[] = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    return null;
  });
  await copyText('AppRafter doctor · prod-eu');
  expect(calls.map((c) => c.cmd)).toEqual(['plugin:clipboard-manager|write_text']);
  expect(calls[0]?.args).toEqual(expect.objectContaining({ text: 'AppRafter doctor · prod-eu' }));
});
