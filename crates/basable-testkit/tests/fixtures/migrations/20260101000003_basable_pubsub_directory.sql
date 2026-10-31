-- migrate:up

-- The framework's second follow-up: the pubsub bus's directory
-- (basable-pubsub, the port of the monorepo's golang/lib/pubsub,
-- 2026-10-31). Which replicas of this project are alive, where each is
-- reached, and which channels each subscribes to; a published message goes
-- from the publisher straight to each subscriber over a connection of their
-- own, never through the database. Every replica heartbeats its row each
-- minute and deletes any row whose heartbeat is older than two minutes, its
-- subscriptions with it. An applied migration is never edited, so this is a
-- file of its own; a project scaffolded before it adds this file to
-- db/app/migrations unchanged.

-- One row per running replica: its id (made at start), the address its peers
-- dial (the pod IP and the port its listener got) and the SHA-256 of the
-- certificate it made at start, the only one its peers accept for it.
CREATE TABLE basable.pubsub_replica (
    id UUID PRIMARY KEY,
    address TEXT NOT NULL,
    certificate_sha256 BYTEA NOT NULL CHECK (octet_length(certificate_sha256) = 32),
    registered_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The sweep's scan.
CREATE INDEX idx_pubsub_replica_heartbeat_at ON basable.pubsub_replica (heartbeat_at);

-- One row per (channel, replica) while that replica has a subscriber on the
-- channel; a publish reads the channel's rows.
CREATE TABLE basable.pubsub_subscription (
    channel TEXT NOT NULL CHECK (channel <> ''),
    replica_id UUID NOT NULL REFERENCES basable.pubsub_replica (id) ON DELETE CASCADE,
    subscribed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (channel, replica_id)
);

-- The cascade from a swept replica.
CREATE INDEX idx_pubsub_subscription_replica_id ON basable.pubsub_subscription (replica_id);

-- The framework pool's login, which the bus runs as, reads and writes both.
GRANT SELECT, INSERT, UPDATE, DELETE ON basable.pubsub_replica, basable.pubsub_subscription TO app;

-- migrate:down
DROP TABLE IF EXISTS basable.pubsub_subscription;
DROP TABLE IF EXISTS basable.pubsub_replica;
