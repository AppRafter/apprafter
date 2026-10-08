// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The only file that imports CSS: components use class names, so `bun test` never loads a
// stylesheet.
import './styles/fonts.css';
import './styles/tokens.css';
import './styles/base.css';
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
