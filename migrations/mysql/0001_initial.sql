-- THUGSred-smsGateway schema, migration 0001 (MySQL / MariaDB).
-- Timestamps are UTC strings in RFC 3339 format with millisecond precision.

CREATE TABLE messages (
    id VARCHAR(64) PRIMARY KEY,
    direction VARCHAR(191) NOT NULL,
    message_type VARCHAR(191) NOT NULL,
    instance_id VARCHAR(191) NOT NULL,
    modem_id VARCHAR(191),
    sim_id VARCHAR(191),
    sender VARCHAR(191),
    recipient VARCHAR(191),
    original_address VARCHAR(191),
    normalized_number VARCHAR(191),
    body MEDIUMTEXT,
    raw_pdu MEDIUMTEXT,
    payload MEDIUMBLOB,
    encoding VARCHAR(191),
    message_class BIGINT,
    protocol_id BIGINT,
    data_coding BIGINT,
    smsc VARCHAR(191),
    concat_ref BIGINT,
    concat_total BIGINT,
    parts BIGINT NOT NULL DEFAULT 1,
    src_port BIGINT,
    dst_port BIGINT,
    network_timestamp VARCHAR(32),
    received_at VARCHAR(32),
    submitted_at VARCHAR(32),
    delivered_at VARCHAR(32),
    status VARCHAR(191) NOT NULL,
    delivery_status VARCHAR(191),
    error MEDIUMTEXT,
    retry_count BIGINT NOT NULL DEFAULT 0,
    metadata MEDIUMTEXT,
    integration_status VARCHAR(191),
    correlation_id VARCHAR(191),
    idempotency_key VARCHAR(191),
    fingerprint VARCHAR(191),
    modem_ref BIGINT,
    delivery_report BIGINT NOT NULL DEFAULT 0,
    created_at VARCHAR(32) NOT NULL,
    updated_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE UNIQUE INDEX idx_messages_idempotency ON messages (idempotency_key);
CREATE UNIQUE INDEX idx_messages_fingerprint ON messages (fingerprint);
CREATE INDEX idx_messages_created ON messages (created_at);
CREATE INDEX idx_messages_direction ON messages (direction, created_at);
CREATE INDEX idx_messages_sender ON messages (sender);
CREATE INDEX idx_messages_recipient ON messages (recipient);
CREATE INDEX idx_messages_normalized ON messages (normalized_number);
CREATE INDEX idx_messages_status ON messages (status);
CREATE INDEX idx_messages_correlation ON messages (correlation_id);
CREATE INDEX idx_messages_modem_ref ON messages (modem_id, modem_ref);

CREATE TABLE message_parts (
    id VARCHAR(64) PRIMARY KEY,
    message_id VARCHAR(64),
    direction VARCHAR(191) NOT NULL,
    modem_id VARCHAR(191),
    address VARCHAR(191),
    concat_ref BIGINT,
    concat_total BIGINT,
    sequence BIGINT NOT NULL DEFAULT 1,
    wide_ref BIGINT NOT NULL DEFAULT 0,
    encoding VARCHAR(191),
    shifts BIGINT NOT NULL DEFAULT 0,
    pdu MEDIUMTEXT,
    payload MEDIUMBLOB,
    tpdu_length BIGINT,
    fingerprint VARCHAR(191),
    status VARCHAR(191) NOT NULL,
    modem_ref BIGINT,
    error MEDIUMTEXT,
    submitted_at VARCHAR(32),
    delivered_at VARCHAR(32),
    created_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE UNIQUE INDEX idx_parts_fingerprint ON message_parts (fingerprint);
CREATE INDEX idx_parts_message ON message_parts (message_id, sequence);
CREATE INDEX idx_parts_pending ON message_parts (direction, status, address, concat_ref);
CREATE INDEX idx_parts_modem_ref ON message_parts (modem_id, modem_ref, status);

CREATE TABLE message_status_history (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    message_id VARCHAR(64) NOT NULL,
    status VARCHAR(191) NOT NULL,
    detail MEDIUMTEXT,
    created_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_history_message ON message_status_history (message_id, id);

CREATE TABLE outbound_queue (
    message_id VARCHAR(64) PRIMARY KEY,
    status VARCHAR(191) NOT NULL,
    priority BIGINT NOT NULL DEFAULT 0,
    modem_id VARCHAR(191),
    claimed_by VARCHAR(191),
    not_before VARCHAR(32),
    expires_at VARCHAR(32),
    attempts BIGINT NOT NULL DEFAULT 0,
    next_attempt_at VARCHAR(32),
    last_error MEDIUMTEXT,
    created_at VARCHAR(32) NOT NULL,
    updated_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_queue_due ON outbound_queue (status, priority, created_at);

CREATE TABLE delivery_reports (
    id VARCHAR(64) PRIMARY KEY,
    message_id VARCHAR(64),
    part_id VARCHAR(64),
    modem_id VARCHAR(191),
    modem_ref BIGINT NOT NULL,
    recipient VARCHAR(191),
    status BIGINT NOT NULL,
    category VARCHAR(191) NOT NULL,
    description MEDIUMTEXT,
    smsc_timestamp VARCHAR(32),
    discharge_time VARCHAR(32),
    raw_pdu MEDIUMTEXT,
    fingerprint VARCHAR(191),
    received_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE UNIQUE INDEX idx_reports_fingerprint ON delivery_reports (fingerprint);
CREATE INDEX idx_reports_message ON delivery_reports (message_id);

CREATE TABLE events (
    id VARCHAR(64) PRIMARY KEY,
    event_type VARCHAR(191) NOT NULL,
    instance_id VARCHAR(191) NOT NULL,
    correlation_id VARCHAR(191),
    data MEDIUMTEXT NOT NULL,
    created_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_events_created ON events (created_at);
CREATE INDEX idx_events_type ON events (event_type, created_at);

CREATE TABLE integration_events (
    id VARCHAR(64) PRIMARY KEY,
    event_id VARCHAR(64) NOT NULL,
    event_type VARCHAR(191) NOT NULL,
    integration VARCHAR(191) NOT NULL,
    status VARCHAR(191) NOT NULL,
    attempts BIGINT NOT NULL DEFAULT 0,
    next_attempt_at VARCHAR(32),
    last_error MEDIUMTEXT,
    created_at VARCHAR(32) NOT NULL,
    updated_at VARCHAR(32) NOT NULL,
    delivered_at VARCHAR(32)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_deliveries_due ON integration_events (status, next_attempt_at);
CREATE INDEX idx_deliveries_integration ON integration_events (integration, created_at);
CREATE INDEX idx_deliveries_event ON integration_events (event_id);

CREATE TABLE integration_attempts (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    delivery_id VARCHAR(64) NOT NULL,
    integration VARCHAR(191) NOT NULL,
    attempt BIGINT NOT NULL,
    success BIGINT NOT NULL,
    status_code BIGINT,
    error MEDIUMTEXT,
    duration_ms BIGINT NOT NULL DEFAULT 0,
    created_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_attempts_delivery ON integration_attempts (delivery_id);
CREATE INDEX idx_attempts_created ON integration_attempts (created_at);

CREATE TABLE dead_letter_queue (
    id VARCHAR(64) PRIMARY KEY,
    kind VARCHAR(191) NOT NULL,
    reference_id VARCHAR(64),
    integration VARCHAR(191),
    event_id VARCHAR(64),
    reason MEDIUMTEXT,
    payload MEDIUMTEXT,
    created_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_dead_created ON dead_letter_queue (created_at);

CREATE TABLE calls (
    id VARCHAR(64) PRIMARY KEY,
    modem_id VARCHAR(191),
    direction VARCHAR(191) NOT NULL,
    number VARCHAR(191),
    state VARCHAR(191) NOT NULL,
    answered BIGINT NOT NULL DEFAULT 0,
    missed BIGINT NOT NULL DEFAULT 0,
    started_at VARCHAR(32) NOT NULL,
    answered_at VARCHAR(32),
    ended_at VARCHAR(32),
    duration_seconds BIGINT,
    termination_reason VARCHAR(191),
    recording_status VARCHAR(191),
    recording_id VARCHAR(64)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_calls_started ON calls (started_at);

CREATE TABLE recordings (
    id VARCHAR(64) PRIMARY KEY,
    call_id VARCHAR(64),
    path MEDIUMTEXT NOT NULL,
    format VARCHAR(191) NOT NULL,
    duration_seconds BIGINT,
    size_bytes BIGINT,
    created_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_recordings_created ON recordings (created_at);

CREATE TABLE attachments (
    id VARCHAR(64) PRIMARY KEY,
    message_id VARCHAR(64),
    filename VARCHAR(191) NOT NULL,
    content_type VARCHAR(191),
    size_bytes BIGINT,
    path MEDIUMTEXT NOT NULL,
    sha256 VARCHAR(191),
    created_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_attachments_message ON attachments (message_id);

CREATE TABLE modem_health (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    modem_id VARCHAR(191) NOT NULL,
    state VARCHAR(191) NOT NULL,
    rssi_dbm BIGINT,
    signal_percent BIGINT,
    registration VARCHAR(191),
    operator VARCHAR(191),
    network_type VARCHAR(191),
    created_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_health_modem ON modem_health (modem_id, created_at);

CREATE TABLE sim_information (
    modem_id VARCHAR(191) PRIMARY KEY,
    sim_id VARCHAR(191),
    iccid VARCHAR(191),
    imsi VARCHAR(191),
    msisdn VARCHAR(191),
    operator VARCHAR(191),
    pin_state VARCHAR(191),
    smsc VARCHAR(191),
    updated_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE ussd_sessions (
    id VARCHAR(64) PRIMARY KEY,
    modem_id VARCHAR(191),
    code VARCHAR(191) NOT NULL,
    response MEDIUMTEXT,
    status VARCHAR(191) NOT NULL,
    created_at VARCHAR(32) NOT NULL,
    completed_at VARCHAR(32)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_ussd_created ON ussd_sessions (created_at);

CREATE TABLE gnss_fixes (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    modem_id VARCHAR(191) NOT NULL,
    latitude DOUBLE NOT NULL,
    longitude DOUBLE NOT NULL,
    altitude DOUBLE,
    speed_kmh DOUBLE,
    heading DOUBLE,
    satellites BIGINT,
    fix_time VARCHAR(32),
    created_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_gnss_created ON gnss_fixes (created_at);

CREATE TABLE audit_events (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    actor VARCHAR(191),
    action VARCHAR(191) NOT NULL,
    target VARCHAR(191),
    detail MEDIUMTEXT,
    request_id VARCHAR(191),
    created_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_audit_created ON audit_events (created_at);

CREATE TABLE automation_log (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    rule_name VARCHAR(191) NOT NULL,
    sender VARCHAR(191),
    message_id VARCHAR(64),
    created_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE INDEX idx_automation_rule ON automation_log (rule_name, created_at);

CREATE TABLE kv_state (
    k VARCHAR(191) PRIMARY KEY,
    v MEDIUMTEXT,
    updated_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
