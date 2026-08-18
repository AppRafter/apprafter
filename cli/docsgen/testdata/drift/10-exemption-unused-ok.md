---
schema-check-ignore:
  - path: "spec.source.path"
    reason: external-tool
    since: v0.9.0
    note: Argo CD's Application CR, whose field set AppRafter does not model
---

# The same exemption, on the page that needs it

Byte-identical front matter. The only difference is that the body names
the path, so the entry has something to silence — a foreign CRD's field,
correctly documented, that our field set neither models nor should.

Argo CD reads `spec.source.path` to find the manifests inside the
repository.
