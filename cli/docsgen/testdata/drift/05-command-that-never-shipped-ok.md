# The same shape, with commands that exist

A bare command path in a shell fence, exactly as the drift twin writes
one. The second line reaches the same command through its **alias**,
which the resolver has to index per parent — `ls` names a different
command under `app`, `backup`, `migration`, `target` and `volume`, so a
flat alias table would have to pick one of them.

```sh
apprafter app list
apprafter app ls
```
