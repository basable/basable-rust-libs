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
  (-1), migrations + Kratos migration (0), app + Kratos + HTTPRoute (1).
- **The frontend is the `static` artifact**, not a workload: the workflow
  uploads the site's files (`index.html` at the root) beside the rendered
  manifests, and the platform puts them into the environment's public
  bucket and serves them at `/` on the environment's hostnames, switching
  the moment the upload lands, before the manifests are applied, and while
  a preview sleeps. A directory's path (`/docs/`, `/docs`) answers that
  directory's `index.html`; a path that names no file answers the root
  `index.html`. The platform sets the security headers. Limits: 16 MiB
  zipped, 64 MiB and 5,000 files unpacked. The HTTPRoute never claims `/`
  (a rule for it would take the site away from the bucket), and nothing
  writes to a `site/` folder of the bucket but the platform, which keeps
  the current commit's site and the one before it.
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
- **Object storage** is `objectStorage: {}` beside an environment's
  `virtual-cluster` block in `.basable/config.yaml`: the platform creates
  that environment's two S3-compatible buckets, a private and a public
  one, and writes their key into the `object-storage` Secret in the
  `default` namespace (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
  `AWS_ENDPOINT_URL_S3`, `AWS_REGION`, `AWS_DEFAULT_REGION`, `S3_BUCKET`
  for the private bucket, `S3_PUBLIC_BUCKET`, and the browser-facing
  `S3_PUBLIC_URL` and `S3_PRIVATE_URL`), read with `envFrom`. Files and
  blobs go there, never on a pod's filesystem or in a PVC. Every object
  in the public bucket is at `$S3_PUBLIC_URL/<key>` (no upload sets an
  ACL: the bucket alone decides what is public); a private one
  reaches a browser as a virtual-hosted presigned URL whose origin is
  replaced with `S3_PRIVATE_URL`, or through the app. The platform owns the
  buckets' configuration. The block's removal deletes the buckets only when
  they are empty or an earlier deploy carried `forceDestroy: true`.
