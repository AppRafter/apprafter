// SPDX-License-Identifier: MIT
//
// Minimal OneBun controller that satisfies the AppRafter
// Application's `expose` contract: `/api/health` for liveness +
// `/api/ready` for readiness. Operators bolt their business
// endpoints onto this same controller (or add new
// `@Controller('/api/...')` files registered in `app.module.ts`).

import { BaseController, Controller, Get } from '@onebun/core';

@Controller('/api')
export class HealthController extends BaseController {
  @Get('/health')
  async health() {
    return {
      status: 'healthy',
      timestamp: new Date().toISOString(),
    };
  }

  @Get('/ready')
  async ready() {
    return {
      ready: true,
      timestamp: new Date().toISOString(),
    };
  }
}
