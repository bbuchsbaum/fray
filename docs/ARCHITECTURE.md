# Fray architecture and acceptance contract

## 1. The minimal model

```
Agent CLI / host hook / runner
             |
      local Unix socket
             |
     one Rust daemon
       mutex + condvar
             |
     one SQLite connection
       WAL, FULL by default
             |
  cards | events | deliveries
        | agents | requests
        | current/history FTS
```

The persistent source of current truth is `cards`; `events` is the audit/replay
stream, not the model's default reading assignment. There is no CRDT, distributed
leader election, filesystem event watcher, search server, or hidden LLM process.
The daemon is the deliberate departure from Mote's daemonless operation-file
model: it provides a resident socket endpoint and condition-variable notification.

The implementation uses Rust's standard library, `rusqlite`, `serde`, `serde_json`,
`clap`, and `fs2`. These are design choices for a small trusted local workload, not
an assertion that threading plus a mutex outperforms every possible alternative.

## 2. Invariants

**Atomic publication.** A successful content operation commits the card update,
event, FTS changes, delivery fan-out, and optional idempotency response in one
SQLite immediate transaction. Notifications happen after that commit. Errors
roll back. Card/event IDs and content revisions are server assigned.

**Read-modify-write safety.** `patch` must name the currently expected content rev.
Only one conflicting patch wins. An annotation does not change the content rev.
A lease acquisition/release does, while renewal does not. Writes to a claimed head
also require the live owner's current fencing token. An expired owner cannot renew
or mutate with its stale token after another agent takes over.

**Separate transport, exposure, handling, and completion.** A stream cursor records
transport progress. `shown_seq` says the adapter emitted context, not that the host
successfully processed it. `ack_seq` records an agent's explicit receipt handling.
Card status records work state. None is silently promoted into another.

**Coalesced attention, retained history.** Each `(agent, card)` has one pending head.
A newer version supersedes the pending version, not its audit history. Acknowledging
an older pending sequence cannot consume a newer one. Previews contain current
heads plus recent annotation excerpts and explicit omission counts. They are not a
claim of semantically lossless compression. Actionable annotation kinds create
independently resolvable questions atomically with the original annotation.

**No subscribe/read gap.** Replay reads and live waiting share the writer's mutex.
The waiter tests its condition while holding that mutex and releases it atomically
when waiting. A write before subscribe is found in durable history; a write after
subscribe wakes the condition. Live `watch` begins at the high-water mark captured
under the same lock unless an explicit replay cursor is provided.

**No slow-subscriber database stall.** A batch is read under the store lock and
written to the socket after releasing it. Batches are capped at 64 events; slow
socket writes time out. Replay is from disk, not an unbounded per-subscriber queue.
A persistent event or query can still consume CPU/IO while serialized under the
mutex; resource caps do not prove a latency SLA.

**Honest onboarding.** A joining agent seeds attention from active, relevant heads,
not all prior events. Its briefing contains current goal/decision context, pending
attention, blockers, its claims, available work, and a roster, with explicit totals
and omissions. Views may overlap to make important items hard to miss. A scope
filter is not a confidentiality boundary. A new unassigned task/question gets a
global discovery delivery to enabled agents even outside topic subscriptions.

**No magical cooperation guarantee.** Fray can make work visible, persistent,
routable, and auditable. It cannot prove an agent understood a report, compel an
agent to take every task, infer whether a current summary is truthful, or solve an
arbitrarily large project within a fixed context budget. It deliberately exposes
these remaining obligations instead of claiming they are transport problems.

## 3. Data and indices

`cards` has stable integer ID, revision, kind, topic, title, summary, status,
priority, pinned flag, tags, author, optional assignee, optional lease owner,
lease expiry, fencing token, creation/update times, and last event sequence.
No arbitrary extension-field schema is required to use it.

`events` has monotonically increasing sequence, time, actor, operation, card ID,
and an immutable-after-commit payload containing the post-operation head and detail.
Annotations are event detail, not a separate mutable discussion tree. Historical
search is a separate FTS5 index; current search indexes only head text and tags.

`deliveries` has agent/card primary key, pending and acknowledgment sequences,
and exposure sequence/time. `agents` has an explicit identity, role, topics,
enabled state, join time, and last-seen time. `requests` caches successful mutation
responses by actor and idempotency key with their exact canonical request.
`meta.store_id` distinguishes database lineages across client reconnects.
Contributors have a delivery row even before receiving a peer event. A zero
pending sequence records participation only; it is excluded from receipt output
and never acknowledges previously unseen updates.

Indices cover active status/priority, topic, ownership, recent sequence, per-card
events, and per-agent pending sequence. Current-state reads do not replay operations.
Memory/response size is bounded by endpoint caps. Disk history, actor registrations,
and completed deliveries are not pruned automatically in this version.

## 4. Protocol

Local trusted JSON-lines protocol; one JSON object per newline. A request is:

```json
{"op":"post","actor":"parser","key":"caller-retry-key-1","args":{"kind":"task","topic":"parser","title":"Fix Unicode","summary":"Acceptance: escaped Unicode tests pass"}}
```

A successful response is `{"ok":true,"data":{...}}`; an error is
`{"ok":false,"error":{"code":"conflict","message":"..."}}`.
The CLI unwraps successful responses for `--json`; raw socket clients receive the
envelope. Clients may reuse a socket. Each request's fields are validated; unknown
fields fail rather than being silently ignored.

Core operations: `ping`, `join`, `leave`, `heartbeat`, `brief`, `post`, `send`, `patch`,
`annotate`, `query`, `show`, `search_history`, `inbox`, `ack`, `expose`, `claim`,
`renew`, `release`, `follow`, `unfollow`, `receipt_status`, `controller`, `agents`,
`wait`, `watch`, and `shutdown`.

`ping` advertises `protocol_version: 2` separately from the package version.
`send` accepts `to`, `body`, optional `title`, `ask`, `priority`, and `refs`.
It transactionally creates a note/question addressed to a registered identity,
using topic `@IDENTITY` and tags for references. `reply` and `thread` are CLI
views over `annotate` and `show`; they introduce no second conversation store.
Bodies in `send`/`annotate` are limited to 8,000 UTF-8 bytes. `send` stores its
complete body in the creation event's `detail.body`, with a bounded summary in
the card head (unchanged 2,000-byte limit). Older post events without `detail.body`
use the historical card summary when rendered by `thread --bodies`. Both file
input and human thread rendering are client conveniences; no attachment store
or schema migration is introduced. `ping.capabilities` advertises `long_messages`,
`inbox_filters`, and `reply_refs`; the client checks required capabilities on the
same connection before sending the operational request.

`watch` takes an optional `after` and topic. It emits a ready frame, ordered events,
and checkpoints, each with store identity and cursor. Topic-filtered checkpoints
can advance past irrelevant events. The CLI reconnects at its last successfully
written cursor and rejects a changed store identity. This is not an acknowledgment
of business processing or a persistent client cursor file.

`wait` returns pending attention immediately or waits for up to the provided
seconds. `--after` intentionally filters pending versions; it is not required for
ordinary inbox draining. The stream's event order and inbox priority order are
different. A consumer should ack handled per-card receipts, not save the last row
of a priority page as a global offset. A cursor above the store's high-water mark
fails explicitly.

Selected limits: requests 128 KiB; client response-frame cap 4 MiB; concurrent
connections 128; regular socket read/write timeout 30 seconds; watch heartbeat
15 seconds; query/history page size <=100; briefing budget 2,000–64,000 UTF-8 bytes.
The heartbeat timeout is a liveness check, not the event delivery interval.

## 5. Attention policy

All cards are public. `topic='*'`, a pinned card, or a steward role reaches all
applicable enabled agents. Otherwise scope subscriptions, explicit assignee,
author, lease owner, and existing card participants determine fan-out. `*` scope
subscriptions exclude `@IDENTITY` topics; explicit topic subscriptions still work.
A new unassigned task or question has global discovery, including when an agent
joins later. Existing participants keep receiving subsequent updates even if
their topics narrow. The publishing actor is registered as a participant without
receiving its own event or acknowledging any unseen peer updates.
Participation is a separate `(agent,card_id)` table, not inferred from deliveries.
`follow` adds participation and seeds the current peer head; `unfollow` removes it
without deleting receipts. A contribution follows again. Scope changes report
retained pending receipts outside the new routing scope. Steward routing stays
global, independent of the runner's narrower attention selection.

`inbox`/`wait` accept `selection=all|involved` (default all). `involved` selects
author, assignee, lease owner, explicit participation, or exact named topic match;
it excludes wildcard/steward-only discovery. The server applies this predicate
inside the condition-variable wait, not as client-side post-filtering. Outgoing
request authors therefore wake for replies too. Nonselected receipts remain intact.
`inbox` additionally accepts boolean `addressed_to_me` and `unresolved` filters.
They intersect selection before counting/pagination: current assignee equals the
actor, and status is not resolved/superseded/withdrawn, respectively. They do not
alter routing, acknowledgment, or the wait/runner selection contract.

Each item includes `receipt={store_id,agent,id,through_seq}`. `ack` accepts the legacy
`id,through` pair or `receipts:[...]`; mixed shapes fail. Batches of up to 100 validate
every store/agent/version inside one transaction. Failure rolls the whole batch back.
`receipt_status` checks acknowledgment of those exact versions, including when a
newer version or higher-priority backlog now exists. Reads and replies never ack.

`leave` disables new subscription/discovery deliveries. Directly addressed messages
and updates to existing conversations continue to queue, including final outcomes
on conversations resolved while the recipient was away. Rejoining refreshes live
heads and retains these receipts. It does not replay closed history for newcomers.

The default scope is `*`. A worker has no special permissions; a steward has broader
attention, not write authority. Assignment is routing, not a claim or exclusive
permission. New questions produced by annotations route to the current owner,
otherwise assignee, otherwise author. Closing the parent does not silently resolve
those questions. Generic dependency DAGs and automatic escalation are not implemented.

`last_seen_ms` is approximate presence, separate from enabled registration and task
lease expiry. Claim TTL defaults to 900 seconds. Expiry is based on wall-clock time;
clock changes affect it. Expiration does not emit a scheduled timer event. A lease
is not a fencing mechanism for Git/filesystem writes outside Fray's own database.
`controllers` holds one run token, state, update time and exit reason per identity.
Begin rejects a fresh waiting/running owner. Updates require the same live token;
120-second expiry permits replacement but rejects stale updates. Leave stops the
controller; heartbeat cannot re-enable a departed identity. `agents` reports both
registration and controller waiting/running/failed/stopped/stale, with `live` derived
from freshness. This is advisory local liveness, not a network authentication fence.

Schema v2 creates participation/controller tables transactionally. A v1 migration
backfills participants from actual event authors once; it preserves all existing
delivery, acknowledgment, exposure, history and store identity. Downgrade is rejected.
All operational CLI requests require protocol v2. The shared client sends a ping
on the same socket before its request (including watch and every reconnect), with
no compatibility cache across connections. Matching protocol versions may have
different package versions. Missing, malformed, older or newer protocol metadata
fails closed before sending the operation. Errors report client and daemon versions
and an explicitly quoted home-specific stop/start command for an older daemon;
a newer daemon instead requires a compatible client. Ping and explicit shutdown
bypass this check for recovery. Start validates even an already-running daemon,
and does not interpret a malformed/failed ping as permission to launch a replacement.
Coordinate an old daemon's stop/start with its owner; installing a new CLI does not
replace the running daemon or restart it automatically. Raw socket integrations
must perform their own handshake; the JSON request envelope has not changed.

## 6. Delivering bytes is not delivering attention

There are three distinct latency budgets:

1. **Publication/delivery:** request -> durable commit -> recipient socket.
2. **Host scheduling:** receipt -> an appropriate host lifecycle boundary or new turn.
3. **Model response:** prompt -> actual inference, reasoning, and action.

The socket/condvar design addresses the first. It cannot make the other two zero.
Printing to a separate pane is not reliable prompt injection into an arbitrary TUI.

`watch_attention` adds a host-neutral scheduling input alongside broadcast `watch`.
It shares inbox selection, starts from pending receipts, and tracks emitted versions
per conversation only within a connection. A reconnect reads durable ACK state again.
Packets are bounded NDJSON with full bodies where possible and explicit omissions;
no output write changes handling state. Selection and condition-variable waiting
share the commit mutex. Fixed-window batching cannot slide forever under traffic.
Socket readiness/heartbeats never enter the CLI's model-facing stdout.
Inbox, wait and stream selection share kind/priority/addressed filters. A kind
matches the card, a pending annotation, or the immutable annotation that created
a linked follow-up. Priority thresholds include smaller (more urgent) values.
Unselected delivery rows remain pending. `wait` accepts JSON `timeout: null` for
an indefinite wait; the CLI spells this `--timeout none`. A connection-scoped EOF
reader cancels it on disconnect, while the commit condition variable supplies
message wakeups. Finite waits preserve their original wire shape and deadline.

An additive `listeners` table fences one transport consumer per identity with
run/connection tokens and a 45-second lease; it cannot overlap a live drive
controller. The daemon clears obsolete socket presence on startup under its lock.
Socket EOF promptly stops a listener through the same condition variable; leases
still bound stale presence when a transport fails without EOF. An expired own
connection is retryable, while explicit leave or replacement remains terminal.
Liveness is observable independently of host inference or task completion.
The optional Claude plugin consumes this generic stream. Codex and other hosts
can consume it through their native scheduling interfaces or use `drive`; no
provider-specific API, credential, prompt inference or scheduling policy is in
the daemon. Detailed packet/lifecycle semantics are in `integrations/README.md`.

The supplied `hook` adapter emits Claude's documented `additionalContext` JSON at
SessionStart and tool boundaries. It marks exposure only after stdout is written,
which allows harmless duplicate delivery if the hook crashes. Pending items remain
unacknowledged until the agent explicitly acks them. An ordinary idle interactive
terminal does not run those hooks just because another agent published something.

The supplied `drive` runner waits before invoking a child, unless `--bootstrap` is
explicit. Routine input is a receipt-only packet; the entire stdin prompt has a
hard 4,000-byte default budget. It trims optional bootstrap context, then trailing
receipts, signaling truncation; if even one receipt cannot fit it fails before
spawning. It does not re-offer acknowledged open questions as available work.
The same involved/all selection controls wait, prompt and remaining-attention checks.
No acknowledged presented receipt stops the run even if undisplayed backlog exists.
Partial progress can continue up to the turn budget. The controller heartbeats every
30 seconds during wait and child execution; default child timeout is 900 seconds.
An unlinked private stdin file avoids blocking forever on a child that never reads.
On failure/timeout it kills and reaps its direct child, not arbitrary descendants.
It handles idle wakeup, not mid-inference preemption or host session management.
Fray hooks detect the runner's `FRAY_DRIVE=1` and emit empty context, preserving the
packet budget. Stderr JSON records prompt bytes, receipts, duration and exit reason;
provider usage is unknown/null, not estimated from prompt length.

The optional experimental `integrations/codex-wake` adapter attaches to an existing
App Server thread. It retains the expected active turn ID for `turn/steer` and uses
`turn/start` when idle. Explicit turn rejection rereads host state; uncertain
delivery stops for inspection. Exact receipt journaling prevents replay after
reconnect without acknowledging work. Bounded exact receipts in the wake message
remain usable after batch-token expiry. Opt-in stream control frames gate dispatch
on listener readiness and stop it on disconnect or listener failure. The existing
host retains approval and sandbox control; approval/input waits defer delivery. Its bounded process owns
and reaps only its Fray watcher. See the adapter README for host requirements and
live qualification limits. An MCP resource-change notification alone still does
not guarantee model wake-up.

## 7. Why not add a chief agent to fix the board?

A chief alone cannot repair a missing delivery contract or an unbounded historical
reading assignment. Making it mandatory creates another waiting point. Fray leaves
claims, visible open questions, and current summaries available without a chief.
A steward can maintain a pinned goal, clarify decisions, inspect old unowned work,
and route unanswered questions. Its work is expressed through the same API and
can be taken over by another appropriately authorized agent or the human.

For Mote coexistence, avoid two authoritative ledgers. Keep Mote's issue/candidate
state authoritative and let Fray carry live questions, coordination decisions,
current handoffs, and references to Mote IDs. A future adapter should be a small
idempotent publisher keyed by `(mote_store_id, op_id)`, not an unrestricted
bidirectional state sync. No adapter, import, repository modification, or migration
is part of this source package.

## 8. Acceptance before relying on it

The included test commands must pass on both Linux and macOS. Verify actual host
hook delivery and runner permissions using two test agents before a real project.
Benchmark release builds on the actual local filesystem with FULL durability,
then repeat at realistic active-card/history sizes and more subscribers. Test slow
readers, process crashes, idle agents, simultaneous claims, obsolete acknowledgments,
long Unicode summaries, and host shutdown. Do not trade away durability silently
to report a better latency number.

Local build/lint/test results are recorded in [VALIDATION.md](VALIDATION.md).
Remaining qualification includes actual-host acceptance, Linux execution, history
retention and backup operations, stronger session lifecycle if identities are
shared, native host adapters where needed, and deployment-specific supervision
and performance tests.

## 9. Future multiple workstations

Keep one authoritative project store initially. A future authenticated network
listener or gateway should dispatch the same `Request` operations to `Store` and
preserve transaction, receipt, idempotency, and replay semantics. Local socket
setup lives in `client.rs`/`server.rs`; collaboration transitions live in `store.rs`
and carry no socket paths or provider SDK objects. There is no speculative network
stack or transport trait in the local version.

References crossing connections are `(store_id, card_id)` or `(store_id, seq)`;
an integer alone is local to its store. Negotiate `protocol_version` and verify
store identity before remote operations. A reconnecting consumer retains its
cursor and applies replay idempotently. Requests with ambiguous outcomes reuse
the same actor/key/payload at the authoritative server.

Remote deployment must add authenticated identity binding, project authorization,
encrypted transport, bounded connections, reconnect/backoff, and cancellation.
Server receipt time governs leases; client clocks do not. Existing names allow
namespaced identities such as `laptop/codex` without encoding model providers into
the data model. A gateway must bind such an identity to its authenticated client,
not trust the local protocol's asserted actor field.

SQLite stays on the server's local disk. Multi-host support is not a shared
network filesystem, copied databases, event merging, or multi-writer offline
replication. Those would require a separate conflict and identity design. Host
adapters run beside their agents and remain responsible for putting received
attention into model context.

## References

- SQLite WAL and durability: https://sqlite.org/wal.html
- SQLite full-text search: https://sqlite.org/fts5.html
- SQLite backups: https://sqlite.org/backup.html
- rusqlite API: https://docs.rs/rusqlite/latest/rusqlite/
- fs2 local file locking: https://docs.rs/fs2/latest/fs2/trait.FileExt.html
- Mote: https://github.com/bbuchsbaum/mote
- Host adapter references: [integrations/README.md](../integrations/README.md).
