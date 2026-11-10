-- Source-only reset for the docker-compose retry test.
-- Each database lives in its own container, so \c cannot cross server boundaries.
-- Destination shards are reset by retry_test.sh via separate psql invocations.
DROP SCHEMA IF EXISTS copy_data CASCADE;
SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots;
\i setup.sql

-- setup.sql copies in about half a second on CI: each table's temporary
-- replication slot lives tens of milliseconds and run.sh can miss them all.
-- A larger log_actions keeps a copy running long enough to kill a shard
-- in the middle of it.
INSERT INTO copy_data.log_actions (tenant_id, action)
SELECT (floor(random() * 10000) + 1)::bigint, 'bulk'
FROM generate_series(1, 300000);
