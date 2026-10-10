// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A DOM for `bun test` (happy-dom), registered before any test file loads.
import { afterEach } from 'bun:test';
import { GlobalRegistrator } from '@happy-dom/global-registrator';

GlobalRegistrator.register();
// Tells React this is a test environment, so act() runs without a warning per call.
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// Testing Library unmounts what a test rendered only when it finds a global afterEach, which
// `bun test` does not always provide: renders then pile up across tests. A hook registered
// here runs after every test in every file. Imported once the DOM exists: its `screen` binds
// to document.body on import.
const { cleanup } = await import('@testing-library/react');
afterEach(cleanup);
