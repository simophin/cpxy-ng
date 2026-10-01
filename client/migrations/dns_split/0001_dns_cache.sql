CREATE TABLE dns_cache (
    name       TEXT    NOT NULL,
    qtype      INTEGER NOT NULL,
    qclass     INTEGER NOT NULL,
    response   BLOB    NOT NULL,
    source     TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (name, qtype, qclass)
);

CREATE INDEX dns_cache_expires_at ON dns_cache (expires_at);
