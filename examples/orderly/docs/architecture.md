# Architecture

One binary, one Postgres, 3 nanoservices. This file is the
map; `routing.yaml` is the topology the build enforces; the plan under
`docs/01-shop/` is the design record it was rendered from.

## Nanoservices and what they own

### `catalog`

Owns the product catalogue.

- Plain tables: `product`.
- Config catalogs: `pricing_rule`.
- Handles: `UpsertProductRequest` → `Product`, `GetProductRequest` → `Product`, `OrderEvent`.
- API: `CatalogService` (UpsertProduct).

### `order`

Drives an order to paid and fulfilled.

- Processing-object types (a lifecycle each, with spec + status tables, an
  adapter, a reconciler and a worker): `order` (`ord_…`), `shipment` (`shp_…`).
- Plain tables: `order_audit`.
- External systems: `payments` (capture_payment: keyed_replay, irreversible).
- Schedules (ticker workers): `sweep_abandoned` every 15m.
- Handles: `EnsureOrderRequest` → `Order`.
- Sends: `GetProductRequest`, `OrderEvent`.
- API: `OrderService` (EnsureOrder).

### `notifier`

Sends one templated email per event.

- External systems: `email` (send_email: keyed_replay, irreversible).
- Handles: `OrderEvent`.

## Topology

| Message | Response | Sent by | Handled by |
|---|---|---|---|
| `UpsertProductRequest` | `Product` |  | `catalog`  |
| `GetProductRequest` | `Product` | `order`  | `catalog`  |
| `OrderEvent` | (event) | `order`  | `catalog` `notifier`  |
| `EnsureOrderRequest` | `Order` |  | `order`  |

`api` sends every API method's request; it handles nothing.

## Deployment shape

- `app/`: the binary, `replicas: 2`, every nanoservice in every replica.
- `frontend/static/`: plain HTML/CSS/JS on nginx-unprivileged.
- `k8s/postgres/`: one CNPG cluster, 1 instance(s), a schema and a role per stateful nanoservice (`nano_catalog`, `nano_order`).
- `k8s/kratos/`: Ory Kratos identity (passwordless email code + passkeys), its own database.
- `k8s/httproute.yaml`: `/api/` → app, `/.ory/` → Kratos, `/` → frontend.

See `docs/platform-contract.md` for the rules the platform enforces on this
tree and `docs/ci.md` for the workflow.
