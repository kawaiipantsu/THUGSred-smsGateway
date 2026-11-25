-- Contacts saved through the dashboard or API.
CREATE TABLE contacts (
    id VARCHAR(64) PRIMARY KEY,
    name VARCHAR(191) NOT NULL,
    number VARCHAR(64) NOT NULL,
    note TEXT,
    created_at VARCHAR(32) NOT NULL,
    updated_at VARCHAR(32) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE UNIQUE INDEX idx_contacts_number ON contacts (number);
