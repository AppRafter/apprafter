// SPDX-License-Identifier: MIT
//
// Typed env config — `bun-http-starter` reads `PORT` (default 3000)
// and `HOST` (default 0.0.0.0). Operators add their own keys here
// + access them via `this.config.get('section.key')` from any
// service / controller.

import { Env, type InferConfigType } from '@onebun/core';

export const envSchema = {
  server: {
    port: Env.number({ default: 3000, env: 'PORT' }),
    host: Env.string({ default: '0.0.0.0', env: 'HOST' }),
  },
};

export type AppConfig = InferConfigType<typeof envSchema>;

declare module '@onebun/core' {
  // eslint-disable-next-line @typescript-eslint/no-empty-object-type
  interface OneBunAppConfig extends AppConfig {}
}
