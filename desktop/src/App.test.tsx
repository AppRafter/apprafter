// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, expect, test } from 'bun:test';
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { App } from './App';

let root: Root | undefined;
afterEach(() => {
  act(() => root?.unmount());
  document.body.replaceChildren();
});

test('the app renders its name', () => {
  const host = document.createElement('div');
  document.body.append(host);
  root = createRoot(host);
  act(() => root?.render(<App />));
  expect(host.textContent).toContain('AppRafter');
});
