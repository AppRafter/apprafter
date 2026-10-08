// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

// Tauri loads http://localhost:1420 in `tauri dev` (tauri.conf.json5 build.devUrl).
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  // macOS 13 ships Safari 16's WebKit; WebView2 is current Chromium.
  build: { target: ['safari16', 'chrome120'], outDir: 'dist' },
});
