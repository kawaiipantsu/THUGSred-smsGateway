-- API bearer tokens managed from the dashboard/API (only the hash is stored).
CREATE TABLE api_tokens (
    id VARCHAR(64) PRIMARY KEY,
    name VARCHAR(191) NOT NULL,
    prefix VARCHAR(32) NOT NULL,
    token_hash VARCHAR(64) NOT NULL,
    scope VARCHAR(16) NOT NULL,
    enabled INT NOT NULL DEFAULT 1,
    created_at VARCHAR(32) NOT NULL,
    last_used_at VARCHAR(32),
    expires_at VARCHAR(32)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE UNIQUE INDEX idx_api_tokens_hash ON api_tokens (token_hash);
