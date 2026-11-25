-- Detected inbound SMS-layer attacks (honeypot classification and statistics).
CREATE TABLE security_events (
    id TEXT PRIMARY KEY,
    message_id TEXT,
    modem_id TEXT,
    sim_id TEXT,
    sender TEXT,
    recipient TEXT,
    category TEXT NOT NULL,
    severity TEXT NOT NULL,
    confidence INTEGER NOT NULL DEFAULT 0,
    detail TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_security_created ON security_events (created_at);
CREATE INDEX idx_security_category ON security_events (category, created_at);
CREATE INDEX idx_security_number ON security_events (recipient, created_at);
