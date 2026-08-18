# A stale Secret key in a kubectl command

**Known hole — this page carries real drift and the gate reports nothing.**

ADR 0046 replaced the composed `DATABASE_URL` key of a `pg` connection
Secret with the decomposed `url` / `user` / `pass` / `host` / `port` /
`db`. The command below therefore prints an empty string against a live
cluster. Nothing here is checkable today: it names no `apprafter`
subcommand, it declares no schema path, and it is not a CUE document, so
every check the gate owns is silent by construction — see the comment on
`a_stale_secret_key_in_a_jsonpath_is_a_known_hole` for what would have to
change.

```sh
kubectl -n demo get secret parser-pg-conn \
  -o jsonpath='{.data.DATABASE_URL}' | base64 -d; echo
```
