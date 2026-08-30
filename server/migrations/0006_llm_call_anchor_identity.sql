ALTER TABLE llm_calls ADD COLUMN projected_message_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE llm_calls ADD COLUMN history_fingerprint TEXT NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS llm_calls_anchor
ON llm_calls(conversation_id, model_hash, status, created_at_ms DESC);
