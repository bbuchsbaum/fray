CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS agents (
    name TEXT PRIMARY KEY,
    role TEXT NOT NULL DEFAULT 'worker',
    topics TEXT NOT NULL DEFAULT '["*"]' CHECK(json_valid(topics)),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),
    joined_ms INTEGER NOT NULL,
    last_seen_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS cards (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    rev INTEGER NOT NULL DEFAULT 1,
    kind TEXT NOT NULL CHECK(kind IN ('goal','task','question','decision','note')),
    topic TEXT NOT NULL,
    title TEXT NOT NULL,
    summary TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open'
      CHECK(status IN ('open','active','blocked','resolved','superseded','withdrawn')),
    priority INTEGER NOT NULL DEFAULT 2 CHECK(priority BETWEEN 0 AND 3),
    pinned INTEGER NOT NULL DEFAULT 0 CHECK(pinned IN (0,1)),
    tags TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(tags)),
    author TEXT NOT NULL REFERENCES agents(name),
    assignee TEXT REFERENCES agents(name),
    lease_owner TEXT REFERENCES agents(name),
    lease_until_ms INTEGER NOT NULL DEFAULT 0,
    fence INTEGER NOT NULL DEFAULT 0,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL,
    last_seq INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS cards_active ON cards(status, priority, id);
CREATE INDEX IF NOT EXISTS cards_topic ON cards(topic, status, priority, id);
CREATE INDEX IF NOT EXISTS cards_owner ON cards(lease_owner, lease_until_ms);
CREATE INDEX IF NOT EXISTS cards_recent ON cards(last_seq DESC);
CREATE INDEX IF NOT EXISTS cards_live_priority ON cards(priority,created_ms,id)
  WHERE status NOT IN ('resolved','superseded','withdrawn');
CREATE TABLE IF NOT EXISTS events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms INTEGER NOT NULL,
    actor TEXT NOT NULL,
    op TEXT NOT NULL,
    card_id INTEGER NOT NULL REFERENCES cards(id),
    payload TEXT NOT NULL CHECK(json_valid(payload))
);
CREATE INDEX IF NOT EXISTS events_card ON events(card_id, seq);
CREATE TABLE IF NOT EXISTS deliveries (
    agent TEXT NOT NULL REFERENCES agents(name),
    card_id INTEGER NOT NULL REFERENCES cards(id),
    pending_seq INTEGER NOT NULL,
    ack_seq INTEGER NOT NULL DEFAULT 0,
    shown_seq INTEGER NOT NULL DEFAULT 0,
    shown_at_ms INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(agent, card_id)
);
CREATE INDEX IF NOT EXISTS deliveries_pending ON deliveries(agent, pending_seq);
-- Receiving a broadcast is not participation. Only contribution or explicit follow is.
CREATE TABLE IF NOT EXISTS participants (
    agent TEXT NOT NULL REFERENCES agents(name),
    card_id INTEGER NOT NULL REFERENCES cards(id),
    PRIMARY KEY(agent, card_id)
);
CREATE TABLE IF NOT EXISTS controllers (
    agent TEXT PRIMARY KEY REFERENCES agents(name),
    run_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('waiting','running','failed','stopped')),
    updated_ms INTEGER NOT NULL,
    reason TEXT
);
-- Muting is reversible attention suppression, never an acknowledgment.
CREATE TABLE IF NOT EXISTS muted_cards (
    agent TEXT NOT NULL REFERENCES agents(name),
    card_id INTEGER NOT NULL REFERENCES cards(id),
    PRIMARY KEY(agent, card_id)
);
CREATE TABLE IF NOT EXISTS requests (
    actor TEXT NOT NULL,
    key TEXT NOT NULL,
    request TEXT NOT NULL,
    response TEXT NOT NULL,
    PRIMARY KEY(actor, key)
);
-- Transport liveness only. A connected listener does not prove host/model activity.
CREATE TABLE IF NOT EXISTS listeners (
    agent TEXT PRIMARY KEY REFERENCES agents(name),
    run_id TEXT NOT NULL,
    connection_id TEXT NOT NULL,
    selection TEXT NOT NULL,
    connected INTEGER NOT NULL CHECK(connected IN (0,1)),
    updated_ms INTEGER NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS card_fts USING fts5(title, summary, tags);
CREATE VIRTUAL TABLE IF NOT EXISTS event_fts USING fts5(text);
CREATE TRIGGER IF NOT EXISTS card_fts_insert AFTER INSERT ON cards BEGIN
    INSERT INTO card_fts(rowid,title,summary,tags) VALUES(new.id,new.title,new.summary,new.tags);
END;
CREATE TRIGGER IF NOT EXISTS card_fts_update AFTER UPDATE OF title,summary,tags ON cards BEGIN
    DELETE FROM card_fts WHERE rowid=old.id;
    INSERT INTO card_fts(rowid,title,summary,tags) VALUES(new.id,new.title,new.summary,new.tags);
END;
-- Immutable records of exactly which receipts a CLI presented to a reader.
-- Presentation is exposure, not handling: nothing here acknowledges.
CREATE TABLE IF NOT EXISTS presented_batches(
    batch TEXT PRIMARY KEY,
    agent TEXT NOT NULL REFERENCES agents(name),
    session TEXT,
    source TEXT NOT NULL CHECK(source IN ('inbox','wait','attention','thread')),
    created_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS presented_batches_agent ON presented_batches(agent, created_ms);
CREATE TABLE IF NOT EXISTS presented_items(
    batch TEXT NOT NULL REFERENCES presented_batches(batch),
    card_id INTEGER NOT NULL REFERENCES cards(id),
    through_seq INTEGER NOT NULL,
    PRIMARY KEY(batch, card_id)
);
-- Which host session currently speaks for an agent name. Collision
-- prevention, not authentication: any caller may assert a session.
-- One host session may speak for several names (e.g. a worker and a fixture).
CREATE TABLE IF NOT EXISTS sessions(
    session TEXT NOT NULL,
    agent TEXT NOT NULL,
    started_ms INTEGER NOT NULL,
    last_seen_ms INTEGER NOT NULL,
    ended_ms INTEGER,
    ended_reason TEXT,
    PRIMARY KEY(session, agent)
);
CREATE INDEX IF NOT EXISTS sessions_agent ON sessions(agent, ended_ms, last_seen_ms);
-- Declared lanes: who is working on which paths. Advisory, never a lock.
CREATE TABLE IF NOT EXISTS lanes(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    agent TEXT NOT NULL,
    paths TEXT NOT NULL CHECK(json_valid(paths)),
    purpose TEXT NOT NULL,
    card_id INTEGER,
    state TEXT NOT NULL CHECK(state IN ('held','queued')),
    created_ms INTEGER NOT NULL,
    released_ms INTEGER,
    released_reason TEXT
);
CREATE INDEX IF NOT EXISTS lanes_live ON lanes(released_ms, agent);
-- One current status line per agent, updated in place.
CREATE TABLE IF NOT EXISTS agent_status(
    agent TEXT PRIMARY KEY,
    text TEXT NOT NULL,
    updated_ms INTEGER NOT NULL
);
PRAGMA user_version = 2;
