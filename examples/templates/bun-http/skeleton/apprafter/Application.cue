// SPDX-License-Identifier: MIT
//
// AppRafter Application manifest. Vets against
// schemas/v1alpha1/application.cue. Commit this file into the
// Git repo Argo CD watches (set as
// Infrastructure.spec.argocd.bootstrapRepo when running
// platform-cli cluster-bootstrap) and the operator reconciles the
// resulting Deployment + Service.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

application: v1alpha1.#Application & {
	metadata: {
		name:      "${{ values.name }}"
		namespace: "${{ values.namespace }}"
	}
	spec: {
		base: {
			image:    "${{ values.image }}"
			replicas: 1
			expose: {
				port:    3000
				public:  false
				network: "internal"
			}
			env: {
				LOG_LEVEL: "info"
			}
		}
		environments: {
			prod: {
				replicas: 3
			}
		}
	}
}
