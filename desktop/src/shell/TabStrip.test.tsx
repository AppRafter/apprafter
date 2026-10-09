// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { TargetTab, View } from '../state/session';
import { TabStrip, type TabStripProps, tabId, tabPanelId } from './TabStrip';

const TABS: TargetTab[] = [
  { key: 'a', target: 'prod-eu', section: 'overview' },
  { key: 'b', target: 'staging', section: 'apps' },
];

function strip(view: View, more: Partial<TabStripProps> = {}) {
  const onShow = mock();
  const onClose = mock();
  const onNewTab = mock();
  const props: TabStripProps = {
    tabs: TABS,
    view,
    os: 'windows',
    running: new Set<string>(),
    onShow,
    onClose,
    onNewTab,
    ...more,
  };
  render(<TabStrip {...props} />);
  return { user: userEvent.setup(), onShow, onClose, onNewTab };
}

describe('TabStrip', () => {
  test('one tab per open target; the shown one is selected', () => {
    strip({ kind: 'tab', key: 'b' });
    const tabs = within(screen.getByRole('tablist')).getAllByRole('tab');
    expect(tabs.map((t) => t.textContent)).toEqual(['prod-eu', 'staging']);
    expect(tabs.map((t) => t.getAttribute('aria-selected'))).toEqual(['false', 'true']);
  });

  test('a click shows a tab; its close button closes it without showing it', async () => {
    const { user, onShow, onClose } = strip({ kind: 'tab', key: 'b' });
    await user.click(screen.getByRole('tab', { name: 'prod-eu' }));
    expect(onShow).toHaveBeenCalledWith({ kind: 'tab', key: 'a' });
    onShow.mockClear();
    await user.click(screen.getByRole('button', { name: 'Close prod-eu' }));
    expect(onClose).toHaveBeenCalledWith('a');
    expect(onShow).not.toHaveBeenCalled();
  });

  test('+ opens the Targets view, says its shortcut, and is pressed while that view shows', async () => {
    const { user, onNewTab } = strip({ kind: 'targets' }, { os: 'macos' });
    const plus = screen.getByRole('button', { name: 'Open a cluster' });
    expect(plus.getAttribute('title')).toBe('Open a cluster (⌘T)');
    expect(plus.getAttribute('aria-pressed')).toBe('true');
    expect(
      within(screen.getByRole('tablist'))
        .getAllByRole('tab')
        .map((t) => t.getAttribute('aria-selected')),
    ).toEqual(['false', 'false']);
    await user.click(plus);
    expect(onNewTab).toHaveBeenCalledTimes(1);
  });

  test('a tab shows unknown health, and no approvals badge at zero', () => {
    strip({ kind: 'tab', key: 'a' });
    const tab = screen.getByRole('tab', { name: 'prod-eu' });
    expect(tab.querySelector('.dot')?.getAttribute('data-tone')).toBe('unknown');
    expect(tab.querySelector('.badge')).toBeNull();
  });

  test('each tab has an id and controls its panel', () => {
    strip({ kind: 'tab', key: 'a' });
    const tab = screen.getByRole('tab', { name: 'prod-eu' });
    expect(tab.id).toBe(tabId('a'));
    expect(tab.getAttribute('aria-controls')).toBe(tabPanelId('a'));
  });

  test('one tab stop: the shown tab, or the first while the Targets view shows', () => {
    strip({ kind: 'tab', key: 'b' });
    const stops = () =>
      within(screen.getByRole('tablist'))
        .getAllByRole('tab')
        .map((t) => t.tabIndex);
    expect(stops()).toEqual([-1, 0]);
    cleanup();
    strip({ kind: 'targets' });
    expect(stops()).toEqual([0, -1]);
  });

  test('the arrow keys, Home and End move to a tab and show it, wrapping at the ends', async () => {
    const { user, onShow } = strip({ kind: 'tab', key: 'a' });
    screen.getByRole('tab', { name: 'prod-eu' }).focus();
    await user.keyboard('{ArrowRight}');
    expect(document.activeElement).toBe(screen.getByRole('tab', { name: 'staging' }));
    expect(onShow).toHaveBeenLastCalledWith({ kind: 'tab', key: 'b' });
    await user.keyboard('{ArrowRight}');
    expect(document.activeElement).toBe(screen.getByRole('tab', { name: 'prod-eu' }));
    expect(onShow).toHaveBeenLastCalledWith({ kind: 'tab', key: 'a' });
    await user.keyboard('{ArrowLeft}');
    expect(onShow).toHaveBeenLastCalledWith({ kind: 'tab', key: 'b' });
    await user.keyboard('{Home}');
    expect(onShow).toHaveBeenLastCalledWith({ kind: 'tab', key: 'a' });
    await user.keyboard('{End}');
    expect(document.activeElement).toBe(screen.getByRole('tab', { name: 'staging' }));
    expect(onShow).toHaveBeenLastCalledWith({ kind: 'tab', key: 'b' });
  });

  test('the shown tab is scrolled into view, so one past the window width is reachable', () => {
    const scrolled: Element[] = [];
    const original = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = function (this: HTMLElement) {
      scrolled.push(this);
    };
    try {
      strip({ kind: 'tab', key: 'b' });
      expect(scrolled).toContain(screen.getByRole('tab', { name: 'staging' }));
    } finally {
      HTMLElement.prototype.scrollIntoView = original;
    }
  });

  test('a spinner replaces the dot while an operation runs on that target', () => {
    strip({ kind: 'tab', key: 'a' }, { running: new Set(['staging']) });
    const busy = screen.getByRole('tab', { name: /staging/ });
    expect(busy.querySelector('.dot')).toBeNull();
    expect(within(busy).getByRole('img', { name: 'An operation is running' })).toBeDefined();
    expect(screen.getByRole('tab', { name: 'prod-eu' }).querySelector('.dot')).not.toBeNull();
  });
});
