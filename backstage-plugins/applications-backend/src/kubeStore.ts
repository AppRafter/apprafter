// SPDX-License-Identifier: MIT
//
// Real `ApplicationStore` impl that reads `apprafter.io/v1alpha1`
// from a kube apiserver. v0.1.34 (sub-phase 1.10b) — pairs with
// the v0.1.33 pure handlers and the v0.1.35 Backstage glue.

import { readFile } from 'node:fs/promises';

import { isApplication, type Application } from './types.ts';
import type { ApplicationStore } from './router.ts';

/** Configuration for `KubeApplicationStore`. */
export interface KubeStoreConfig {
  /** Full URL to the kube apiserver, e.g. `https://kubernetes.default.svc`. */
  apiServer: string;
  /** Bearer token presented in the `Authorization` header. */
  token: string;
  /**
   * Custom fetch impl — tests inject a mock. Defaults to
   * `globalThis.fetch` at call-time.
   */
  fetchImpl?: typeof fetch;
  /**
   * PEM-encoded CA cert. When set, passed to fetch via the Bun
   * `tls.ca` option so the apiserver's self-signed cert validates.
   */
  caCert?: string;
}

const SERVICE_ACCOUNT_DIR = '/var/run/secrets/kubernetes.io/serviceaccount';

/**
 * Build a config from the standard in-cluster service-account
 * mount (`/var/run/secrets/kubernetes.io/serviceaccount/`) and the
 * `KUBERNETES_SERVICE_HOST` / `KUBERNETES_SERVICE_PORT_HTTPS` env
 * vars. Throws when not running in-cluster.
 */
export async function inClusterConfig(): Promise<KubeStoreConfig> {
  const host = process.env.KUBERNETES_SERVICE_HOST;
  if (!host) {
    throw new Error(
      'KUBERNETES_SERVICE_HOST is not set; not running in-cluster',
    );
  }
  const port = process.env.KUBERNETES_SERVICE_PORT_HTTPS ?? '443';
  const apiServer = `https://${host}:${port}`;
  const token = (
    await readFile(`${SERVICE_ACCOUNT_DIR}/token`, 'utf8')
  ).trim();
  const caCert = await readFile(`${SERVICE_ACCOUNT_DIR}/ca.crt`, 'utf8');
  return { apiServer, token, caCert };
}

/** Real `ApplicationStore` backed by a kube apiserver. */
export class KubeApplicationStore implements ApplicationStore {
  constructor(private readonly config: KubeStoreConfig) {}

  async list(namespace?: string): Promise<Application[]> {
    const url = namespace
      ? `${this.config.apiServer}/apis/apprafter.io/v1alpha1/namespaces/${namespace}/applications`
      : `${this.config.apiServer}/apis/apprafter.io/v1alpha1/applications`;
    const response = await this.fetcher()(url, this.requestInit());
    if (!response.ok) {
      throw new Error(
        `kube list applications ${response.status}: ${await response.text()}`,
      );
    }
    const data = (await response.json()) as { items?: unknown[] };
    return (data.items ?? []).filter(isApplication);
  }

  async get(namespace: string, name: string): Promise<Application | null> {
    const url = `${this.config.apiServer}/apis/apprafter.io/v1alpha1/namespaces/${namespace}/applications/${name}`;
    const response = await this.fetcher()(url, this.requestInit());
    if (response.status === 404) return null;
    if (!response.ok) {
      throw new Error(
        `kube get application ${response.status}: ${await response.text()}`,
      );
    }
    const data = (await response.json()) as unknown;
    return isApplication(data) ? data : null;
  }

  private fetcher(): typeof fetch {
    return this.config.fetchImpl ?? globalThis.fetch;
  }

  private requestInit(): RequestInit {
    const init: RequestInit & { tls?: { ca: string } } = {
      headers: {
        Authorization: `Bearer ${this.config.token}`,
        Accept: 'application/json',
      },
    };
    if (this.config.caCert) {
      init.tls = { ca: this.config.caCert };
    }
    return init;
  }
}
