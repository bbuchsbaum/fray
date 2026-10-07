CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- Outbound request journals and observations, never a second Mote ledger.
CREATE TABLE IF NOT EXISTS mote_operations (
    actor TEXT NOT NULL, key TEXT NOT NULL, kind TEXT NOT NULL,
    payload TEXT NOT NULL CHECK(json_valid(payload)), digest TEXT NOT NULL,
    session TEXT, rev INTEGER NOT NULL, state TEXT NOT NULL,
    observations TEXT NOT NULL CHECK(json_valid(observations)),
    result TEXT CHECK(result IS NULL OR json_valid(result)),
    created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL,
    PRIMARY KEY(actor,key)
);
CREATE TABLE IF NOT EXISTS mote_review_subjects (
    card_id INTEGER PRIMARY KEY REFERENCES cards(id),
    store_id TEXT NOT NULL, candidate_id TEXT NOT NULL,
    proposer TEXT NOT NULL, observed_candidate TEXT NOT NULL CHECK(json_valid(observed_candidate)),
    predecessor_card INTEGER, successor_card INTEGER
);
CREATE TABLE IF NOT EXISTS mote_feed_claims (
    store_id TEXT NOT NULL, entity TEXT NOT NULL, holder TEXT, op_id TEXT NOT NULL,
    PRIMARY KEY(store_id,entity)
);
CREATE TABLE IF NOT EXISTS mote_claim_events (
    store_id TEXT NOT NULL, op_id TEXT NOT NULL, PRIMARY KEY(store_id,op_id)
);
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
-- First routing time is independent of the age of the card's content.
-- NULL means a legacy receipt predating this table; do not invent its time.
CREATE TABLE IF NOT EXISTS delivery_routes(
    agent TEXT NOT NULL,
    card_id INTEGER NOT NULL,
    routed_at_ms INTEGER,
    PRIMARY KEY(agent,card_id),
    FOREIGN KEY(agent,card_id) REFERENCES deliveries(agent,card_id) ON DELETE CASCADE
);
CREATE TRIGGER IF NOT EXISTS deliveries_first_route AFTER INSERT ON deliveries BEGIN
    INSERT OR IGNORE INTO delivery_routes(agent,card_id,routed_at_ms)
    VALUES(new.agent,new.card_id,CAST((SELECT value FROM meta WHERE key='routing_clock_ms') AS INTEGER));
END;
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
-- A drive run's own diagnostics: its owned child process group, how that
-- group was disposed of, and urgent attention queued behind a running turn.
CREATE TABLE IF NOT EXISTS controller_details (
    agent TEXT PRIMARY KEY REFERENCES agents(name),
    run_id TEXT NOT NULL,
    detail TEXT NOT NULL
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
-- Displayed peer generations are session-local exposure, never card ACKs.
CREATE TABLE IF NOT EXISTS peer_generations(
    agent TEXT PRIMARY KEY REFERENCES agents(name),
    generation INTEGER NOT NULL CHECK(generation>0),
    session TEXT,
    joined_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS peer_seen(
    reader TEXT NOT NULL REFERENCES agents(name),
    session TEXT NOT NULL,
    peer TEXT NOT NULL REFERENCES agents(name),
    generation INTEGER NOT NULL CHECK(generation>0),
    PRIMARY KEY(reader,session,peer)
);
-- Review subjects and verdicts are conversation evidence, not acceptance.
CREATE TABLE IF NOT EXISTS review_subjects(
    card_id INTEGER PRIMARY KEY REFERENCES cards(id),
    baseline TEXT NOT NULL,
    candidate TEXT NOT NULL,
    subject_rev INTEGER NOT NULL CHECK(subject_rev>0),
    mote_ref TEXT
);
CREATE TABLE IF NOT EXISTS review_verdicts(
    event_seq INTEGER PRIMARY KEY REFERENCES events(seq),
    card_id INTEGER NOT NULL REFERENCES cards(id),
    reviewer TEXT NOT NULL REFERENCES agents(name),
    subject_rev INTEGER NOT NULL,
    version TEXT NOT NULL,
    verdict TEXT NOT NULL CHECK(verdict IN ('approve','object','blocked'))
);
CREATE INDEX IF NOT EXISTS review_verdicts_card ON review_verdicts(card_id,event_seq);
-- Superseded by session_waits (below); kept so older boards open unchanged.
CREATE TABLE IF NOT EXISTS agent_waits(
    agent TEXT PRIMARY KEY,
    session TEXT,
    refreshed_ms INTEGER NOT NULL
);
-- Each wait in progress with no card, kind, priority, addressed or unresolved
-- filter, one row per wait: they wake for anything assigned (reachability "wakeable",
-- docs/design/no-silent-stalls.md R1). Additive, so older boards gain it.
CREATE TABLE IF NOT EXISTS wake_waits(
    wait_id TEXT PRIMARY KEY,
    agent TEXT NOT NULL,
    refreshed_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS wake_waits_agent ON wake_waits(agent);
-- Mote events already turned into attention, once per recipient
-- (docs/design/mote-adapter.md section 6). Additive: a board without Mote
-- never writes here.
CREATE TABLE IF NOT EXISTS mote_events(
    store_id TEXT NOT NULL,
    key TEXT NOT NULL,
    recipient TEXT NOT NULL,
    card_id INTEGER REFERENCES cards(id),
    PRIMARY KEY(store_id, key, recipient)
);
-- The last Mote claim holder Fray has seen per entity: seeded from Mote's
-- board on first sync, then kept by the claim events in the same transaction
-- as the cursor. It only decides who hears that a claim changed hands.
CREATE TABLE IF NOT EXISTS mote_claims(
    store_id TEXT NOT NULL,
    entity TEXT NOT NULL,
    holder TEXT,
    op_id TEXT NOT NULL,
    PRIMARY KEY(store_id, entity)
);
-- For attention that reports a subject's current state (a candidate's
-- landability): the last state delivered per recipient. A card goes out
-- whenever the state differs from the last one delivered, so a return to an
-- earlier state is reported too (review of 05dfbc2).
-- Mote requests (msg_kind request) carded for their addressee, tracked by
-- state (docs/design/no-silent-stalls.md R2): an open request becomes an ask
-- card once; when Mote shows it answered, that card is settled.
CREATE TABLE IF NOT EXISTS mote_requests(
    store_id TEXT NOT NULL,
    msg_id TEXT NOT NULL,
    recipient TEXT NOT NULL,
    card_id INTEGER NOT NULL,
    state TEXT NOT NULL,
    PRIMARY KEY(store_id, msg_id, recipient)
);
-- Open Mote requests to actors who never joined this board: they cannot
-- be carded for their addressee, so R3 escalates them. Refreshed by each
-- request sync; a row not seen for two hours is no longer open.
CREATE TABLE IF NOT EXISTS mote_requests_unknown(
    store_id TEXT NOT NULL,
    msg_id TEXT NOT NULL,
    recipient TEXT NOT NULL,
    sender TEXT NOT NULL,
    first_seen_ms INTEGER NOT NULL,
    last_seen_ms INTEGER NOT NULL,
    PRIMARY KEY(store_id, msg_id, recipient)
);
-- Escalations of stuck requests (docs/design/no-silent-stalls.md R3): one
-- card per stuck request, reason and steward, whoever ticks.
CREATE TABLE IF NOT EXISTS escalations(
    id INTEGER PRIMARY KEY,
    subject TEXT NOT NULL,
    reason TEXT NOT NULL,
    recipient TEXT NOT NULL,
    card_id INTEGER NOT NULL,
    created_ms INTEGER NOT NULL,
    settled_ms INTEGER
);
CREATE INDEX IF NOT EXISTS escalations_open ON escalations(subject, reason, recipient, settled_ms);
CREATE TABLE IF NOT EXISTS mote_subjects(
    store_id TEXT NOT NULL,
    subject TEXT NOT NULL,
    recipient TEXT NOT NULL,
    state TEXT NOT NULL,
    -- A final state (a candidate landed, superseded or abandoned) is sticky:
    -- a slower sync can never replace it with an older pending state.
    final INTEGER NOT NULL DEFAULT 0 CHECK(final IN (0,1)),
    PRIMARY KEY(store_id, subject, recipient)
);
-- Keepalives the daemon started (docs/design/keepalive.md): one per name.
-- Its session may bind beside the name's interactive session (`companion`).
-- Usage is the day's input tokens (UTC days since the epoch) against budget.
-- Additive, so older boards gain it.
CREATE TABLE IF NOT EXISTS keepalives(
    agent TEXT PRIMARY KEY REFERENCES agents(name),
    session TEXT NOT NULL,
    companion TEXT NOT NULL,
    host TEXT NOT NULL CHECK(host IN ('claude','codex')),
    cwd TEXT NOT NULL,
    log TEXT NOT NULL,
    pid INTEGER,
    budget INTEGER NOT NULL,
    usage_day INTEGER NOT NULL DEFAULT 0,
    usage_tokens INTEGER NOT NULL DEFAULT 0,
    stop_requested INTEGER NOT NULL DEFAULT 0 CHECK(stop_requested IN (0,1)),
    started_ms INTEGER NOT NULL
);
-- An interactive terminal's turns, per name and host session, from `fray
-- hook` (docs/design/keepalive.md, "One turn at a time"): busy from
-- UserPromptSubmit until a Stop that does not block, StopFailure,
-- SessionEnd or Codex's Interrupt. A busy mark with no hook activity for
-- 30 minutes is stale. `prompts` counts UserPromptSubmit, so a keepalive
-- can tell the terminal has taken a turn since its fork; `transcript` is
-- the host's transcript path from the hook input. Never written by a
-- keepalive's own session. Additive, so older boards gain it.
CREATE TABLE IF NOT EXISTS terminal_turns(
    agent TEXT NOT NULL,
    session TEXT NOT NULL,
    busy_since_ms INTEGER,
    last_hook_ms INTEGER NOT NULL,
    prompts INTEGER NOT NULL DEFAULT 0,
    transcript TEXT,
    PRIMARY KEY(agent, session)
);
-- Replies a keepalive posted as its agent, until the terminal is told of
-- them at its next prompt ("while you were away"). Additive.
CREATE TABLE IF NOT EXISTS keepalive_actions(
    event_seq INTEGER PRIMARY KEY,
    agent TEXT NOT NULL,
    card_id INTEGER NOT NULL,
    kind TEXT NOT NULL,
    follow_up INTEGER,
    reported INTEGER NOT NULL DEFAULT 0 CHECK(reported IN (0,1))
);
CREATE INDEX IF NOT EXISTS keepalive_actions_unreported ON keepalive_actions(agent, reported);
-- A wait in progress, per name and session (no session: ''), refreshed
-- every minute: reachability, not activity. Replaces agent_waits, which
-- kept one row per name, so a keepalive's waits and its terminal's no
-- longer share a row. Additive; agent_waits is left unused.
CREATE TABLE IF NOT EXISTS session_waits(
    agent TEXT NOT NULL,
    session TEXT NOT NULL DEFAULT '',
    refreshed_ms INTEGER NOT NULL,
    PRIMARY KEY(agent, session)
);
PRAGMA user_version = 3;
-- Owner-selected options are stored beside the keepalive's fixed identity.
CREATE TABLE IF NOT EXISTS keepalive_options(
    agent TEXT PRIMARY KEY REFERENCES agents(name),
    model TEXT
);
