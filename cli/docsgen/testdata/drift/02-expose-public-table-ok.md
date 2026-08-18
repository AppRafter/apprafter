# The same table, naming the field that exists

The drifted row replaced by a real one in the same shape: a backticked
dotted token in column one, a backticked type in column two. The word
`public` is still here — it is a legitimate *value* of the field — so a
lexical ban on the token would fail this page too.

| Field            | Type                                       | Notes                             |
| ---------------- | ------------------------------------------ | --------------------------------- |
| `expose.port`    | `int & >0 & <=65535`                       | Container port to expose.         |
| `expose.network` | `"public" \| "internal" \| "vpn"`          | Network visibility for the route. |
