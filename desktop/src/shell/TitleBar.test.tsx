// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { emit } from '@tauri-apps/api/event';
import { clearMocks, mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { Os } from '../ipc/generated/Os';
import { settleIpc } from '../test/settle';
import { TitleBar } from './TitleBar';

let calls: string[];
let maximized: boolean;

beforeEach(() => {
  calls = [];
  maximized = false;
  mockWindows('main');
  mockIPC(
    (cmd) => {
      calls.push(cmd);
      if (cmd === 'plugin:window|is_maximized') return maximized;
      return null;
    },
    { shouldMockEvents: true },
  );
});

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

// Unmount first, and let the caption buttons' unlisten reach the mocks before they go.
afterEach(async () => {
  cleanup();
  await settleIpc();
  clearMocks();
});

function bar(os: Os) {
  const { container, unmount } = render(
    <TitleBar os={os}>
      <div>tabs</div>
    </TitleBar>,
  );
  return { header: container.querySelector('.titlebar') as HTMLElement, unmount };
}

describe('TitleBar', () => {
  test('caption buttons on Windows only', () => {
    bar('windows');
    expect(screen.getByRole('button', { name: 'Minimize' })).toBeDefined();
    expect(screen.getByRole('button', { name: 'Maximize' })).toBeDefined();
    expect(screen.getByRole('button', { name: 'Close' })).toBeDefined();
  });

  test.each(['macos', 'linux'] as const)('no caption buttons on %s', (os) => {
    bar(os);
    expect(screen.queryByRole('button', { name: 'Minimize' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Close' })).toBeNull();
  });

  test('macOS reserves the traffic lights their place, and only macOS', () => {
    const mac = bar('macos');
    expect(mac.header.querySelector('.traffic-inset')).not.toBeNull();
    mac.unmount();
    expect(bar('windows').header.querySelector('.traffic-inset')).toBeNull();
  });

  test('the bar drags the window on Windows and macOS; Linux keeps its own title bar', () => {
    for (const os of ['windows', 'macos'] as const) {
      const { header, unmount } = bar(os);
      expect(header.getAttribute('data-tauri-drag-region')).toBe('true');
      expect(header.querySelector('.titlebar-spacer')?.getAttribute('data-tauri-drag-region')).toBe(
        'true',
      );
      // The logo is an SVG inside the zone: the whole zone drags.
      expect(header.querySelector('.logo-zone')?.getAttribute('data-tauri-drag-region')).toBe(
        'deep',
      );
      unmount();
    }
    const linux = bar('linux');
    expect(linux.header.querySelectorAll('[data-tauri-drag-region]')).toHaveLength(0);
  });

  test('the caption buttons minimize, maximize, and quit through the app', async () => {
    bar('windows');
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: 'Minimize' }));
    await user.click(screen.getByRole('button', { name: 'Maximize' }));
    await user.click(screen.getByRole('button', { name: 'Close' }));
    expect(calls).toContain('plugin:window|minimize');
    expect(calls).toContain('plugin:window|toggle_maximize');
    expect(calls).toContain('quit');
    expect(calls).not.toContain('plugin:window|close');
  });

  test('maximized, the middle button restores, and swaps back on the next resize', async () => {
    maximized = true;
    bar('windows');
    await act(settle);
    expect(screen.getByRole('button', { name: 'Restore' })).toBeDefined();
    maximized = false;
    await act(async () => {
      await emit('tauri://resize', { width: 1024, height: 700 });
      await settle();
    });
    expect(screen.getByRole('button', { name: 'Maximize' })).toBeDefined();
  });
});
