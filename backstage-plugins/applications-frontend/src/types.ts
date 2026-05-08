// SPDX-License-Identifier: MIT
//
// Frontend-side mirror of the Application types declared in
// `@apprafter/applications-backend`. Hand-synced — change here
// when you change there. The duplication is intentional so each
// package can be published independently of the other.

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
