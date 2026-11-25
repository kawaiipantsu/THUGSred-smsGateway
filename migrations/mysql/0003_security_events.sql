-- Detected inbound SMS-layer attacks (honeypot classification and statistics).
CREATE TABLE security_events (
    id VARCHAR(64) PRIMARY KEY,
    message_id VARCHAR(64),
    modem_id VARCHAR(191),
    sim_id VARCHAR(191),
    sender VARCHAR(191),
    recipient VARCHAR(191),
    category VARCHAR(32) NOT NULL,
    severity VARCHAR(16) NOT NULL,
    confidence INT NOT NULL DEFAULT 0,
    detail MEDIUMTEXT,
    created_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_security_created ON security_events (created_at);
CREATE INDEX idx_security_category ON security_events (category, created_at);
CREATE INDEX idx_security_number ON security_events (recipient, created_at);
