// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The only file that imports CSS: components use class names, so `bun test` never loads a
// stylesheet.
import './styles/fonts.css';
import './styles/tokens.css';
import './styles/base.css';
import './styles/components.css';
import './styles/shell.css';
import './styles/flows.css';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';

// `bun run dev:mock`: a browser stands in for the Rust side. The import is dynamic so a build
// without the variable leaves the mock (and @tauri-apps/api/mocks, and the fixtures) out of the
// bundle.
if (import.meta.env.VITE_MOCK_IPC === '1') {
  const { installMockIpc, mockOptionsFromUrl } = await import('./ipc/mock');
  installMockIpc(mockOptionsFromUrl(window.location.search));
}

const host = document.getElementById('root');
if (host === null) throw new Error('index.html has no #root');
createRoot(host).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
