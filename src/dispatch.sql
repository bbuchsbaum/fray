-- Attention and observed results; Mote alone owns claims and reservations.
CREATE TABLE IF NOT EXISTS dispatch_offers (
 id INTEGER PRIMARY KEY REFERENCES cards(id), sender TEXT NOT NULL,
 issue TEXT NOT NULL, tag TEXT NOT NULL, generation INTEGER NOT NULL,
 status TEXT NOT NULL, offered_to TEXT, attempted TEXT NOT NULL CHECK(json_valid(attempted)),
 offer_ttl_s INTEGER NOT NULL, offer_until_ms INTEGER NOT NULL, claim_ttl_s INTEGER NOT NULL,
 accepted_by TEXT, attempt_key TEXT, attempt_session TEXT, attempt_peer_generation INTEGER, attempt_started_ms INTEGER,
 observed_claim_token TEXT, observed_lease_until TEXT, progress_marker TEXT,
 created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS dispatch_handoffs (
 id INTEGER PRIMARY KEY REFERENCES cards(id), source_card INTEGER NOT NULL,
 sender TEXT NOT NULL, recipient TEXT NOT NULL, issue TEXT NOT NULL, operation_key TEXT NOT NULL,
 payload TEXT NOT NULL CHECK(json_valid(payload)), status TEXT NOT NULL,
 receipt TEXT CHECK(receipt IS NULL OR json_valid(receipt)), updated_ms INTEGER NOT NULL,
 UNIQUE(sender,operation_key)
);
