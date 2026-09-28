# Operations

## Secrets

- **Database**: CNPG mints the `app-postgres-app` Secret (`username`,
  `password`, `uri`) when it bootstraps the cluster; the migration Job and
  the app read `uri`. Nobody generates a password; rotation is CNPG's.
- **Kratos**: the cookie-signing secret is generated inside the cluster by
  `k8s/kratos/secret-job.yaml` (wave -2) on the first deploy and kept
  thereafter: the Job creates the `kratos-secrets` Secret only when it is
  absent, so it exists nowhere but the cluster. Deleting the Secret makes
  the next deploy mint a new one, which signs every user out. Kratos
  reaches its own `kratos` database as the CNPG `app` login (the same
  Secret as above); there is no cipher secret because OIDC token
  encryption is not used.
- **Mail**: the Kratos courier's SMTP connection is a user-supplied secret
  the platform's secrets input path will carry; until then sign-in codes
  appear in the Kratos pod's logs only.
- **Provider API keys** (payments, email, anything under
  `externalSystems`): user-supplied secrets, added through the platform's
  secrets input path when it ships; until then, a repository Actions secret
  rendered into a Kubernetes Secret by a workflow step.

## Database

One CNPG cluster, 1 instance(s), 5Gi. The PVC is the only
copy until a backup to object storage is configured (`backup.barmanObjectStore`
+ a `ScheduledBackup`, needing an S3 credential). HA is `instances: 3` in
`k8s/postgres/cluster.yaml` within a paid quota; nothing else changes.

Preview sleep stops the pods; the PVC and the CNPG Secrets survive.

## Logs and health

JSON logs to stdout (`kubectl logs`). `/readyz` is false until every pool
answers and the migration ledger holds every embedded version; `/healthz` is
liveness. `/metrics` exposes the effect counters
(`externaleffect_attempts_total{operation,verdict}` and friends).
