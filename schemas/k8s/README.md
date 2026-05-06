# schemas/k8s/

Imported Kubernetes API types (core/apps/networking) that AppRafter
CRDs reference. Generated via `cue import` from the upstream
Kubernetes OpenAPI.

This directory is intentionally empty in the initial scaffold. It
will be populated in phase 1.7, when the Application operator's
renderer needs concrete `Deployment`, `Service`, and Gateway-API
types to construct child resources from `Application` manifests.

Until then, the v1alpha1 schemas use a minimal local `#ObjectMeta`
(see `schemas/v1alpha1/types.cue`) and reference Kubernetes types
by name only.
