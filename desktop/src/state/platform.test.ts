// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import { osFromUserAgent } from './platform';

test('the user agent names the OS each webview runs on', () => {
  // WebView2, WKWebView, WebKitGTK.
  expect(
    osFromUserAgent(
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36 Edg/140.0.0.0',
    ),
  ).toBe('windows');
  expect(
    osFromUserAgent(
      'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)',
    ),
  ).toBe('macos');
  expect(
    osFromUserAgent('Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko)'),
  ).toBe('linux');
  expect(osFromUserAgent('')).toBe('linux');
});
