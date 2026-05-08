// SPDX-License-Identifier: MIT
//
// TypeScript mirror of operator-core::Application — kept in sync
// with operator/operator-core/src/application.rs by hand. The
// invariants we care about (apiVersion / kind constants, optional
// fields, env override map shape) are exercised by types.test.ts.

export interface Application {
  apiVersion: 'apprafter.io/v1alpha1';
  kind: 'Application';
  metadata: ObjectMeta;
  spec: ApplicationSpec;
  status?: ApplicationStatus;
}

export interface ObjectMeta {
  name: string;
  namespace?: string;
  uid?: string;
  generation?: number;
  labels?: Record<string, string>;
  annotations?: Record<string, string>;
}

export interface ApplicationSpec {
  base?: ApplicationBaseSpec;
  environments?: Record<string, ApplicationBaseSpec>;
}

export interface ApplicationBaseSpec {
  image?: string;
  replicas?: number;
  expose?: ApplicationExpose;
  env?: Record<string, string>;
}

export interface ApplicationExpose {
  port: number;
  public?: boolean;
  network?: 'public' | 'internal' | 'vpn';
}

export interface ApplicationStatus {
  phase?: string;
  observedGeneration?: number;
  conditions?: ApplicationCondition[];
  endpointURL?: string;
}

export interface ApplicationCondition {
  type: string;
  status: string;
  lastTransitionTime: string;
  reason: string;
  message: string;
  observedGeneration?: number;
}

/**
 * Minimal type guard — shape-checks the API constants without
 * walking every nested field. Used by the router to drop
 * non-Application JSON the kube apiserver might surface.
 */
export function isApplication(obj: unknown): obj is Application {
  if (typeof obj !== 'object' || obj === null) return false;
  const o = obj as Record<string, unknown>;
  return (
    o.apiVersion === 'apprafter.io/v1alpha1' &&
    o.kind === 'Application' &&
    typeof o.metadata === 'object' &&
    o.metadata !== null
  );
}
