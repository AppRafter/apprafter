# A drifted field in a field table

A table cell is not a fence and not a paragraph, and the scanner bounds a
table row as a block of its own. The identifier check runs page-wide
precisely so a row like the last one below is still judged.

| Field            | Type                     | Notes                              |
| ---------------- | ------------------------ | ---------------------------------- |
| `expose.port`    | `int & >0 & <=65535`     | Container port to expose.          |
| `expose.public`  | `bool` (default `false`) | Whether to create a public route.  |
