// SPDX-License-Identifier: FSL-1.1-Apache-2.0
/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** `1` under `bun run dev:mock`: the mock IPC stands in for Rust (src/ipc/mock). */
  readonly VITE_MOCK_IPC?: string;
}
