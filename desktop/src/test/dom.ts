// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A DOM for `bun test` (happy-dom), registered before any test file loads.
import { GlobalRegistrator } from '@happy-dom/global-registrator';

GlobalRegistrator.register();
// Tells React this is a test environment, so act() runs without a warning per call.
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
