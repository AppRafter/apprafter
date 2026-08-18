# The same heredoc, using the field that exists

One key changed. The fence is still shell, still holds a heredoc, still
mixes a `kubectl` command line with YAML the structural scanner has to
read past — so a green run here says the check separates the field name
from everything around it.

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
      network: internal
YAML
```
