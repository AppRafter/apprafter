// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The clipboard, write-only (capabilities/main.json5 grants write_text alone): a doctor report or
// an install command goes out; nothing is ever read back (D.3 overview R11).
import { writeText } from '@tauri-apps/plugin-clipboard-manager';

export function copyText(text: string): Promise<void> {
  return writeText(text);
}
