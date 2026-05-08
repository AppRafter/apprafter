// SPDX-License-Identifier: MIT
//
// Bootstrap entry. `OneBunApplication` reads `PORT` (default
// 3000) from envSchema and binds the registered routes; metrics +
// tracing are enabled out of the box so the AppRafter operator
// can scrape `/metrics` once a Service points at this pod.

import { OneBunApplication } from '@onebun/core';

import { AppModule } from './app.module.ts';
import { envSchema } from './config.ts';

const SERVICE_NAME = '${{ values.name }}';

const app = new OneBunApplication(AppModule, {
  envSchema,
  metrics: { enabled: true },
  tracing: { enabled: true, serviceName: SERVICE_NAME },
});

app
  .start()
  .then(() => {
    const logger = app.getLogger({ className: 'AppBootstrap' });
    const url = app.getHttpUrl();
    logger.info(`${SERVICE_NAME} listening on ${url}`);
  })
  .catch((error: unknown) => {
    const logger = app.getLogger({ className: 'AppBootstrap' });
    logger.error(
      'Failed to start application',
      error instanceof Error ? error : new Error(String(error)),
    );
    process.exit(1);
  });
