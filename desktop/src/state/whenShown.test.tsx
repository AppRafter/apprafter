// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, expect, mock, test } from 'bun:test';
import { act, cleanup, render } from '@testing-library/react';
import { Activity } from 'react';
import { newScope, resetLifecycle, sessionScope } from '../ipc/lifecycle';
import { ScopeContext } from './scope';
import { useWhenShown } from './whenShown';

afterEach(() => {
  cleanup();
  resetLifecycle();
});

function setup() {
  const apply = mock();
  const orElse = mock();
  const screen = newScope(sessionScope());
  let when: (value: string) => void = () => {};
  function Probe() {
    when = useWhenShown<string>(apply, orElse);
    return null;
  }
  const Host = ({ shown }: { shown: boolean }) => (
    <ScopeContext value={screen.scope}>
      <Activity mode={shown ? 'visible' : 'hidden'}>
        <Probe />
      </Activity>
    </ScopeContext>
  );
  const view = render(<Host shown />);
  return {
    apply,
    orElse,
    screen,
    give: (value: string) => act(() => when(value)),
    show: (shown: boolean) => view.rerender(<Host shown={shown} />),
  };
}

test('shown, an end is applied at once, and once', () => {
  const { apply, orElse, give } = setup();
  give('done');
  expect(apply.mock.calls).toEqual([['done']]);
  expect(orElse).not.toHaveBeenCalled();
});

test('hidden, an end waits for the next show', () => {
  const { apply, give, show } = setup();
  show(false);
  give('done');
  expect(apply).not.toHaveBeenCalled();
  show(true);
  expect(apply.mock.calls).toEqual([['done']]);
  show(false);
  show(true);
  expect(apply).toHaveBeenCalledTimes(1);
});

test('its screen gone first, the end goes to orElse and is never applied', () => {
  const { apply, orElse, screen, give, show } = setup();
  show(false);
  give('done');
  screen.end();
  expect(orElse.mock.calls).toEqual([['done']]);
  show(true);
  expect(apply).not.toHaveBeenCalled();
});

test('given after its screen went, the end goes to orElse at once', () => {
  const { apply, orElse, screen, give } = setup();
  screen.end();
  give('late');
  expect(orElse.mock.calls).toEqual([['late']]);
  expect(apply).not.toHaveBeenCalled();
});
