// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A DOM for `bun test` (happy-dom), registered before any test file loads.
import { afterEach, beforeEach } from 'bun:test';
import { GlobalRegistrator } from '@happy-dom/global-registrator';
import { resetLifecycle } from '../ipc/lifecycle';

GlobalRegistrator.register();
// Tells React this is a test environment, so act() runs without a warning per call.
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// Testing Library unmounts what a test rendered only when it finds a global afterEach, which
// `bun test` does not always provide: renders then pile up across tests. A hook registered
// here runs after every test in every file. Imported once the DOM exists: its `screen` binds
// to document.body on import.
const { cleanup } = await import('@testing-library/react');
afterEach(cleanup);

// bun runs every test file in one process, so the session scope (ipc/lifecycle.ts) is shared by
// all of them: what a test leaves registered on it — a read still running, a held plan, a draft —
// would be ended by the next lock any later test makes, into that test's own mock, and satisfy
// its assertions with another file's ids. Every test starts and ends with a fresh session. The
// swap ends nothing, so its order among a file's own hooks does not matter: what registers while
// a file's teardown settles goes to the scope it captured, never to the next test's.
beforeEach(resetLifecycle);
afterEach(resetLifecycle);
