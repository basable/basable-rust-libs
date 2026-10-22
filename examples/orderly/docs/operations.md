# Operations

## Secrets

- **Database**: CNPG mints the `app-postgres-app` Secret (`username`,
  `password`, `uri`) when it bootstraps the cluster; the migration Job and
  the app read `uri`. Nobody generates a password; rotation is CNPG's.
- **Kratos**: the cookie-signing secret (`cookie-secret`) and the cipher
  secret (`cipher-secret`, 32 characters: Kratos encrypts its OIDC state and
  tokens with it) are generated inside the cluster by
  `k8s/kratos/secret-job.yaml` (wave -2) on the first deploy and kept
  thereafter: the Job creates the `kratos-secrets` Secret only when it is
  absent (and adds a missing `cipher-secret` to one), so it exists nowhere
  but the cluster. Deleting the Secret makes the next deploy mint new ones,
  which signs every user out. Kratos reaches its own `kratos` database as
  the CNPG `app` login (the same Secret as above).
- **Mail**: the Kratos courier reads its SMTP connection from the
  `app-secrets` key `SMTP_CONNECTION_URI` (`k8s/kratos/deployment.yaml`, an
  OPTIONAL `secretKeyRef`, so the first deploy does not wait for it). Add it
  in the editor's Secrets action (an `smtps://user:pass@host:465/` URI);
  until then the ConfigMap's localhost placeholder applies and sign-in codes
  appear in the Kratos pod's logs only.
- **Sign in with Basable**: the platform writes the `basable-login` Secret
  (`BASABLE_LOGIN_ISSUER`, `BASABLE_LOGIN_CLIENT_ID`,
  `BASABLE_LOGIN_CLIENT_SECRET`) into every environment's cluster and keeps
  it there: an OpenID client of Basable's own sign-in, one per environment,
  whose callbacks are this app's hostnames. Kratos's `basable` provider is
  built from it (`k8s/kratos/deployment.yaml`), so the sign-in page offers
  "Sign in with Basable" with no setup and no SMTP: single sign-on with
  every app on Basable, the person's verified email copied into the
  identity's traits (`oidc.basable.jsonnet`, which refuses a sign-in
  whose id token carries no verified email). The person stays a user of
  THIS app; email codes work on the same identity. Never declare that
  Secret in `k8s/`.
- **User-supplied secrets** (a provider's API key, a webhook's
  verification secret, anything under `externalSystems`): each lives in the
  `app-secrets` Secret in the `default` namespace of each environment's
  cluster, and the app reads it with its own non-optional `secretKeyRef` in
  `k8s/app/deployment.yaml` (never `envFrom`); the deploy waits at its
  secrets step until the key exists, so a missing one is never silent. The
  reference is added by whoever fills the code that reads the secret (the
  provider in `provider.rs`, a webhook's `verify`), who names the key in the
  chat so the user can add it. The user adds the value in the editor (the
  environment's Secrets action, or Add secrets on a deploy waiting at its
  secrets step); preview and live hold separate values, and the platform
  stores none of them. Never put a value in the repository, a manifest or a
  workflow.
- **Webhooks**: a declared webhook is a raw route under `/api/webhooks/`
  whose `verify` refuses every delivery until the provider's own scheme is
  implemented there (a signature, a token, a certificate, an API lookup);
  the scaffold assumes nothing about it.

## Database

One CNPG cluster, 1 instance(s), 5Gi. The PVC is the only
copy until the cluster backs up to the environment's private bucket: declare
`objectStorage` for the environment in `.basable/config.yaml`, then add a
`backup.barmanObjectStore` (destination
`s3://OBJECT_STORAGE_BUCKET_PLACEHOLDER/cnpg/<cluster>`, endpoint
`OBJECT_STORAGE_ENDPOINT_PLACEHOLDER`, both filled in by the deploy per
environment; credentials from the `object-storage` Secret; a
`retentionPolicy`) and a `ScheduledBackup` to `k8s/postgres/cluster.yaml`.
Base backups and the WAL archive then go to the bucket. HA is `instances: 3`
in `k8s/postgres/cluster.yaml` within a paid quota; nothing else changes.

Preview sleep stops the pods; the PVC and the CNPG Secrets survive.

## Logs and health

JSON logs to stdout (`kubectl logs`). `/readyz` is false until every pool
answers and the migration ledger holds every embedded version; `/healthz` is
liveness. `/metrics` exposes the effect counters
(`externaleffect_attempts_total{operation,verdict}` and friends).
