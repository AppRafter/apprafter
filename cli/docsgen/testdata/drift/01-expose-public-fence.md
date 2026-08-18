# A drifted field in a complete manifest

This fence is a complete CUE document — a `package` clause and the schema
import — so it owes the CUE check as well as the page-wide identifier
check. Both must object.

```cue
package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

app: v1alpha1.#Application & {
    metadata: name: "parser"
    spec: base: {
        image: "ghcr.io/my-org/parser:1.0.0"
        expose: {
            port:   8080
            public: false
        }
    }
}
```
