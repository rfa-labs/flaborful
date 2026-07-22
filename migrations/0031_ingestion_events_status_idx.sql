-- Snapshot reads (agent + company ingestion status) do:
--   SELECT DISTINCT ON (repo) ... WHERE owner_kind=$1 AND agent_id=$2 AND kind='status' AND repo<>''
--   ORDER BY repo, created_at DESC
-- and an EXISTS check for the 'complete' sentinel. The existing (owner_kind, agent_id, created_at)
-- index doesn't cover the kind='status' filter or the per-repo grouping, so these scanned all of an
-- owner's rows. A partial index on the status rows makes them index-only and keeps the completion
-- prune (which also targets status rows per repo) cheap.
CREATE INDEX IF NOT EXISTS ingestion_events_status_idx
    ON ingestion_events (owner_kind, agent_id, repo, created_at DESC)
    WHERE kind = 'status';
