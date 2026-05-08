// SPDX-License-Identifier: MIT
//
// Root module that registers every controller / provider for the
// service. Operators add new modules to `imports` and new
// controllers to `controllers` as the service grows.

import { Module } from '@onebun/core';

import { HealthController } from './health.controller.ts';

@Module({
  imports: [],
  controllers: [HealthController],
  providers: [],
})
export class AppModule {}
