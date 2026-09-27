# The platform contract

What the basable platform does with this repository, and the rules every
file under `k8s/` and `.github/workflows/` must keep.

- **The platform deploys the `rendered-manifests` artifact the workflow
  uploads, and nothing else.** It never reads `k8s/` from the repository.
  A push to `dev` deploys the preview environment
  (`*.preview.basable.test`), a push to `main` the live one. The
  deployment waits on the newest workflow run for the commit, so the
  repository ships exactly ONE workflow file.
- **A docs-only push deploys nothing.** The workflow ignores `docs/**` and
  the platform creates no deployment for a push whose every changed path is
  under `docs/`; that is how plan commits stay free.
- **Images are pinned by digest.** `k8s/app/deployment.yaml` keeps the
  literal image path `registry.test.local/abandon-above-tilt/shop`; the workflow's Render step replaces
  the placeholder tag with the digest of the image the run pushed, matched
  by NAME. Never use `:latest` anywhere under `k8s/`; the lint step refuses it.
- **Apply waves.** `basable.com/apply-wave` (an integer in quotes, lower
  first, negative allowed) gates a wave on the previous wave's Deployments
  and StatefulSets being ready, its Jobs completed, and any custom resource
  that reports a `Ready` condition (a CNPG `Cluster`) being Ready; a failed
  Job fails the deployment. This tree: CNPG operator + Cluster + the Kratos secret Job (-2), the kratos Database
  (-1), migrations + Kratos migration (0), app + frontend + Kratos + HTTPRoute (1).
- **A Secret read through `secretKeyRef` must exist when the pod starts**:
  `CreateContainerConfigError` is terminal for the health gate. The wave
  layout guarantees it (the Cluster is Ready before wave 0, and CNPG writes `app-postgres-app` during bootstrap;
  the secret Job's completion proves `kratos-secrets`).
- **`TENANT_DOMAIN_PLACEHOLDER`** is replaced by the platform with the
  environment's hostname; the HTTPRoute's parent is
  `basable-gateway/tenant-gateway`, section `https`.
- **Pod security is `restricted`**: non-root numeric users, no privilege
  escalation, all capabilities dropped, `RuntimeDefault` seccomp — as every
  manifest here already does. PVCs omit `storageClassName`; Services are
  ClusterIP; a NetworkPolicy here would be inert.
- **Sizing** is `.basable/config.yaml`, per environment, at the free-tier
  maximum for the organisation's first month; above it, a payment method is
  required.
