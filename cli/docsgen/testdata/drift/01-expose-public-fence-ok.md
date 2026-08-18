# The same manifest, using the field that exists

The drift fixture's manifest with `public: false` replaced by the shipped
`network:` key. Everything else about the fence — the package clause, the
import, the anchor depth, the tag — is the same, so a green run here is
about the field name and nothing else.

```cue
package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

app: v1alpha1.#Application & {
    metadata: name: "parser"
    spec: base: {
        image: "ghcr.io/my-org/parser:1.0.0"
        expose: {
            port:    8080
            network: "internal"
        }
    }
}
```
