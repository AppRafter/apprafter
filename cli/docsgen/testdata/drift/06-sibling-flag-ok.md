# The same flag, on a command that takes it

The token is identical — `--namespace demo`, in a shell fence, after a
two-level command path. Only the command it is attached to differs, which
is the whole distinction the check has to draw: a gate that reported this
line would be a gate nobody leaves switched on.

```sh
apprafter app add --namespace demo --name parser
```
