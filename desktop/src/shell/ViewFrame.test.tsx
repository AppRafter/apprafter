// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { clearMocks } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect } from 'react';
import { newScope, resetLifecycle, sessionScope } from '../ipc/lifecycle';
import { resetOperations } from '../ipc/operations';
import { useRead } from '../state/read';
import { ScopeContext, useScope } from '../state/scope';
import { type Harness, installHarness } from '../test/ipc';
import { settleIpc } from '../test/settle';
import { type ShowOverlay, useOverlay, ViewFrame } from './ViewFrame';

let h: Harness;
beforeEach(() => {
  h = installHarness();
});
afterEach(async () => {
  cleanup();
  await settleIpc();
  resetLifecycle();
  resetOperations();
  clearMocks();
});

/** An overlay that starts a read that keeps running, with a Close. */
function Reading({ opId, onClose }: { opId: number; onClose: () => void }) {
  const { run } = useRead<unknown>();
  useEffect(() => {
    void run(async () => opId);
  }, [run, opId]);
  return (
    <div role="dialog" aria-label="Reading">
      <button type="button" onClick={onClose}>
        Close
      </button>
    </div>
  );
}

function renderFrame(scope = sessionScope()) {
  let show: ShowOverlay = () => {};
  function Opener() {
    show = useOverlay();
    return null;
  }
  render(
    <ScopeContext value={scope}>
      <ViewFrame>
        <Opener />
      </ViewFrame>
    </ScopeContext>,
  );
  return { open: (render: Parameters<ShowOverlay>[0]) => act(() => show(render)) };
}

test("an overlay's close ends its scope: what it started is cancelled", async () => {
  const opId = h.newOperation([]);
  const { open } = renderFrame();
  open((close) => <Reading opId={opId} onClose={close} />);
  await waitFor(() => expect(h.of('op_subscribe')).toHaveLength(1));
  await userEvent.setup().click(screen.getByRole('button', { name: 'Close' }));
  expect(screen.queryByRole('dialog', { name: 'Reading' })).toBeNull();
  expect(h.of('op_cancel').map((c) => c.args)).toEqual([{ opId }]);
});

test('an overlay goes with its view: the view scope ending ends it', async () => {
  const view = newScope(sessionScope());
  const opId = h.newOperation([]);
  const { open } = renderFrame(view.scope);
  open((close) => <Reading opId={opId} onClose={close} />);
  await waitFor(() => expect(h.of('op_subscribe')).toHaveLength(1));
  view.end();
  expect(h.of('op_cancel').map((c) => c.args)).toEqual([{ opId }]);
});

test('each overlay has a scope of its own, under the view', () => {
  const view = newScope(sessionScope());
  const seen: { gone: () => boolean }[] = [];
  function Probe() {
    seen.push(useScope());
    return null;
  }
  const { open } = renderFrame(view.scope);
  open(() => <Probe />);
  const [own] = seen;
  expect(own).toBeDefined();
  expect(own).not.toBe(view.scope);
  expect(own?.gone()).toBe(false);
  view.end();
  expect(own?.gone()).toBe(true);
});
