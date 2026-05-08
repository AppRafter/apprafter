// SPDX-License-Identifier: MIT
import { test, expect } from 'bun:test';
import { applicationsForEnvironment, environmentsOf } from './helpers.ts';
import type { Application } from './types.ts';

function appWithEnvs(name: string, envs: string[]): Application {
  const environments: Record<string, object> = {};
  for (const e of envs) environments[e] = {};
  return {
    apiVersion: 'apprafter.io/v1alpha1',
    kind: 'Application',
    metadata: { name },
    spec: { environments },
  };
}

test('environmentsOf returns sorted unique env names across apps', () => {
  const apps = [
    appWithEnvs('a', ['prod', 'dev']),
    appWithEnvs('b', ['staging', 'prod']),
    appWithEnvs('c', []),
  ];
  expect(environmentsOf(apps)).toEqual(['dev', 'prod', 'staging']);
});

test('environmentsOf returns empty array when no app declares envs', () => {
  const apps = [appWithEnvs('a', []), appWithEnvs('b', [])];
  expect(environmentsOf(apps)).toEqual([]);
});

test('applicationsForEnvironment filters apps that declare the env override', () => {
  const apps = [
    appWithEnvs('a', ['prod']),
    appWithEnvs('b', ['dev']),
    appWithEnvs('c', ['prod', 'dev']),
  ];
  const prod = applicationsForEnvironment(apps, 'prod');
  expect(prod.map((a) => a.metadata.name)).toEqual(['a', 'c']);
});

test('applicationsForEnvironment returns empty when env is missing everywhere', () => {
  const apps = [appWithEnvs('a', ['prod']), appWithEnvs('b', ['dev'])];
  expect(applicationsForEnvironment(apps, 'staging')).toEqual([]);
});
