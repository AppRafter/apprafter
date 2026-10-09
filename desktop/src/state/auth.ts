// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// How the UI names the OS mechanism that confirms the device owner (AppInfo.auth.method):
// "Confirm with Windows Hello next.", "Ask for your account password before…".
import type { AuthMethod } from '../ipc/generated/AuthMethod';

const PROMPTS: Record<AuthMethod, string> = {
  windows_hello: 'Windows Hello',
  windows_credential: 'your Windows password',
  mac_local_authentication: 'Touch ID or your Mac password',
  polkit: 'your account password',
  pam: 'your account password',
  fake: 'the test build prompt',
};

export function authPrompt(method: AuthMethod): string {
  return PROMPTS[method];
}
