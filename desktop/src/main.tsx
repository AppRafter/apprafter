// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';

const host = document.getElementById('root');
if (host === null) throw new Error('index.html has no #root');
createRoot(host).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
