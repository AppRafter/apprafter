// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The browser smoke of the shell on the mock IPC (`bun run dev:mock`): Chromium and WebKit, in
// the dark and the light theme, and once with Windows chrome. `bun run e2e` runs it under node:
// Playwright's runner on bun is not reliable. Screenshots go into the report; nothing compares
// them pixel by pixel.
import { defineConfig, devices, type Project } from '@playwright/test';

const PORT = 1420;
const ci = process.env.CI !== undefined;
const desktop = new URL('..', import.meta.url).pathname;
const viewport = { width: 1280, height: 800 };

const BROWSERS = [
  ['chromium', devices['Desktop Chrome']],
  ['webkit', devices['Desktop Safari']],
] as const;
const THEMES = ['dark', 'light'] as const;

const projects: Project[] = BROWSERS.flatMap(([browser, device]) =>
  THEMES.map((theme) => ({
    name: `${browser}-${theme}`,
    use: { ...device, viewport },
    metadata: { query: `?theme=${theme}` },
  })),
);
// The caption buttons render on Windows only.
projects.push({
  name: 'chromium-windows',
  use: { ...devices['Desktop Chrome'], viewport },
  metadata: { query: '?theme=dark&os=windows' },
});

export default defineConfig({
  testDir: '.',
  outputDir: 'test-results',
  forbidOnly: ci,
  retries: ci ? 1 : 0,
  reporter: [['list'], ['html', { outputFolder: 'playwright-report', open: 'never' }]],
  use: { baseURL: `http://localhost:${PORT}`, trace: 'retain-on-failure' },
  webServer: {
    command: 'bun run dev:mock',
    cwd: desktop,
    port: PORT,
    reuseExistingServer: !ci,
    timeout: 120_000,
  },
  projects,
});
