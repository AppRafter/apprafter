# A flag borrowed from a sibling command

`--namespace` is real on a dozen commands, which is what makes this the
shape a reader copies and a reviewer misses. `app status` is not one of
them: the namespace is fixed by `app add` and read back off the Argo CD
Application.

```sh
apprafter app status parser --namespace demo
```
