# The two shapes that are correct

Both of these write the same token as the drift twin and both are true,
which is why the recurrence guard cannot be lexical: a ban on
`DATABASE_URL` would fail this page and leave the stale jsonpath standing.

An application picks the env-var name itself and binds it to a claim
field — the name is the app's, not the platform's:

```cue
spec: base: {
    needs: pg: {}
    env: {
        DATABASE_URL: claim.pg.url
    }
}
```

And a `jsonpath` that reads the env-var **name** off the rendered
Deployment is reading the app's own choice, not a Secret key, so it stays
correct:

```sh
kubectl -n demo get deployment parser -o \
  jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="DATABASE_URL")].valueFrom.secretKeyRef.key}{"\n"}'
```
