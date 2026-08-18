# A drifted field inside a kubectl heredoc

This is the case an apply-and-see check cannot reach: the apiserver
**prunes** an unknown key rather than rejecting it, so a reader who pastes
this block gets a successful apply and a silently dropped setting.
Membership over the fence's structure is the only thing that sees it.

```sh
kubectl apply -f - <<'YAML'
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: parser
  namespace: demo
spec:
  base:
    image: ghcr.io/my-org/parser:1.0.0
    expose:
      port: 8080
      public: true
YAML
```
