// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, test } from 'bun:test';
import { INITIAL_SESSION, type Session, sessionReducer } from './session';

const open = (state: Session, target: string, key = `key-${target}`) =>
  sessionReducer(state, { type: 'openTarget', target, key });

const three = () => open(open(open(INITIAL_SESSION, 'prod-eu'), 'staging'), 'lab');

describe('sessionReducer', () => {
  test('starts on the Targets view with no tab', () => {
    expect(INITIAL_SESSION).toEqual({ tabs: [], view: { kind: 'targets' } });
  });

  test('opening a target appends its tab on Overview and shows it', () => {
    const state = open(INITIAL_SESSION, 'prod-eu', 'a');
    expect(state.tabs).toEqual([{ key: 'a', target: 'prod-eu', section: 'overview' }]);
    expect(state.view).toEqual({ kind: 'tab', key: 'a' });
  });

  test('opening a target that has a tab shows that tab, with its section kept', () => {
    let state = open(INITIAL_SESSION, 'prod-eu', 'a');
    state = sessionReducer(state, { type: 'navigate', key: 'a', section: 'apps' });
    state = sessionReducer(state, { type: 'show', view: { kind: 'targets' } });
    state = open(state, 'prod-eu', 'b');
    expect(state.tabs).toEqual([{ key: 'a', target: 'prod-eu', section: 'apps' }]);
    expect(state.view).toEqual({ kind: 'tab', key: 'a' });
  });

  test('closing the shown tab shows the one now at its place, else the one before', () => {
    let state = sessionReducer(three(), {
      type: 'show',
      view: { kind: 'tab', key: 'key-staging' },
    });
    state = sessionReducer(state, { type: 'closeTab', key: 'key-staging' });
    expect(state.view).toEqual({ kind: 'tab', key: 'key-lab' });
    state = sessionReducer(state, { type: 'closeTab', key: 'key-lab' });
    expect(state.view).toEqual({ kind: 'tab', key: 'key-prod-eu' });
  });

  test('closing the last tab shows the Targets view', () => {
    let state = open(INITIAL_SESSION, 'prod-eu', 'a');
    state = sessionReducer(state, { type: 'closeTab', key: 'a' });
    expect(state).toEqual({ tabs: [], view: { kind: 'targets' } });
  });

  test('closing a tab that is not shown leaves the view alone', () => {
    let state = three();
    state = sessionReducer(state, { type: 'closeTab', key: 'key-prod-eu' });
    expect(state.view).toEqual({ kind: 'tab', key: 'key-lab' });
    expect(state.tabs.map((t) => t.target)).toEqual(['staging', 'lab']);
  });

  test('a rename keeps the tab and its key, so nothing remounts', () => {
    const state = sessionReducer(three(), { type: 'targetRenamed', from: 'staging', to: 'stage' });
    expect(state.tabs[1]).toEqual({ key: 'key-staging', target: 'stage', section: 'overview' });
  });

  test('removing a target closes its tab', () => {
    const state = sessionReducer(three(), { type: 'targetRemoved', target: 'lab' });
    expect(state.tabs.map((t) => t.target)).toEqual(['prod-eu', 'staging']);
    expect(state.view).toEqual({ kind: 'tab', key: 'key-staging' });
  });

  test('a view or tab that does not exist changes nothing', () => {
    const state = three();
    expect(sessionReducer(state, { type: 'show', view: { kind: 'tab', key: 'nope' } })).toBe(state);
    expect(sessionReducer(state, { type: 'closeTab', key: 'nope' })).toBe(state);
    expect(sessionReducer(state, { type: 'navigate', key: 'nope', section: 'apps' })).toBe(state);
    expect(sessionReducer(state, { type: 'targetRemoved', target: 'nope' })).toBe(state);
  });
});
