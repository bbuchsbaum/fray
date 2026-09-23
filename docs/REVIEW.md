# Vision and source review

## Attention and durable waits (2026-09-23)

Outcome: host-neutral selected attention with bounded packets and an optional
Claude interactive adapter. Codex and other hosts retain the generic CLI/drive
contract; no provider SDK or model logic enters the daemon.
Scope: indefinite `wait`, distinct exit status, durable receipt resume, shared
kind/priority filters, `watch --attention`, fixed-window batching,
rich receipt packets, reconnect recovery, fenced listener presence, adapter/docs.
Invariants: stdout contains attention only; transport/exposure never acknowledges;
restart recovers pending versions; priority pagination never becomes a global ACK;
one live consumer per identity; listener liveness is not model responsiveness.
Paths: src/attention.rs, server/client/store/main, integrations, tests and IPC suite.
Acceptance: cargo fmt/check/clippy/test/build with locked dependencies; Python IPC
and SQL gates; real-host qualification separately recorded, with no fabricated
claim from a synthetic host. No live shared daemon or global installation changed.
Risks: additive capability required on the daemon; monitor availability differs
by host/version. No Git metadata exists in this source copy.
Independent Claude review reproduced two defects: near-budget packet construction
could shrink urgent content or terminate a stream; a killed idle watcher held its
lease until the next heartbeat. The fixes defer later items before shrinking the
sole item and use socket EOF to release listener presence. Regression checks cover
379 boundary sizes and immediate replacement after SIGKILL. Lease renew events
are excluded from conversation context, and an own lease expired over sleep can
reconnect without reviving an explicit leave. Exact gate and real-host evidence is
in `VALIDATION.md`. Plugin auto-load remains separate from native Monitor delivery.

## Collaboration feedback follow-up (2026-09-22)

The StoryModel/StoryAtlas feedback favors contract-first work across two owners,
batched questions, and independent consumer tests. Within one module, one writer
and a bounded cold review can cost less than a commit-by-commit conversation.
The optional `fray-seam` and `fray-review` skills package those behaviors without
starting peers, claiming ownership, or granting landing authority.

Implemented here: 8,000-byte send bodies stored in immutable history with bounded
heads; file input for send/reply; readable paginated `thread --bodies`; addressed
and unresolved inbox filters that retain hidden receipts; read-only `find` for
workspace boards; named/bundle skill installation with conflict preflight.
No schema or dependency changes. Capability checks protect old protocol-2 daemons.

Assessment of the remaining requests:

| Request | Current support and remaining work |
| --- | --- |
| Wake a peer on a direct question | `drive --selection involved` already wakes an idle managed child for pending direct attention. It does not interrupt an existing interactive host; an urgent bit alone cannot supply that adapter. Keep `agents` controller state visible and use a drive launch for each participating noninteractive worker. |
| First-class review of a commit | The optional review skill standardizes exact repo/base/SHA, evidence and explicit verdicts. A typed Fray review item and automatic stale detection remain unimplemented. Mote's installed CLI already provides candidate propose/review/show/supersede; a future Fray adapter should route candidate attention while Mote retains acceptance authority. |
| Queued gate mutex | Existing claim/renew/release leases are fenced, but have no named gate queue, blocking acquisition, or holder-leave release. A real mutex needs atomic FIFO acquisition, cancellation/disconnect handling, TTL/renewal, stale-owner fencing and status. Process execution must honor fencing or stop on lease loss; TTL expiry alone cannot prove the old gate stopped. |
| Longer bodies / attachments | Send/reply accept 8,000 UTF-8 bytes and `--body-file`; the full initial body survives summary edits and restarts. Arbitrary binary attachments are not implemented. |
| Workspace board discovery | `find ..` lists nearby existing homes and diagnostic reachability; existing ancestor `.fray` resolution shares a workspace home. No merging or automatic change to running peers. |
| Readable threads | `thread ID --bodies` lists ordered sequence, author, kind and full text, with an explicit continuation cursor. |
| Inbox noise | `--addressed-to-me --unresolved` narrows pending conversations; `--selection involved` also retains outgoing replies/participation. Hidden receipts remain durable. |
| Mote link-through | Existing `reply --ref` adds searchable references. Automatic notes mirroring remains unimplemented: it needs explicit destination/actor, idempotent pointer publication, and visible partial-failure recovery across two stores. |

The tests use temporary stores and deterministic hosts. No live pilot daemon,
global binary, or installed project skill was changed. This source folder has no
Git metadata or Mote store, so these are local changes and an assessment, not a
published release or filed tracker backlog.

## Compatibility follow-up (0.2.1)

The updated PLSNeuro pilot confirmed CLI/daemon skew beyond drive: inbox/wait sent
v2 fields to a live v1 daemon and start reported success. The shared client now
checks protocol on the same socket before operational requests, including each
watch reconnect. Ping/shutdown remain recovery paths. Start rejects incompatible
or malformed live peers without launching a replacement. Diagnostics include both
versions and a quoted home-specific restart command, or advise upgrading the client
for a newer daemon. Protocol 2 remains compatible across 0.2.x package versions;
no schema/dependency changes. Verified with 64 Rust tests, 29 IPC tests, 22 SQL
checks, formatting, strict Clippy and a release build. Running daemons untouched.

## Pilot fixes 1–5 (2026-09-21)

Outcome: inexpensive, demand-driven collaboration after the PLSNeuro pilot.
Scope: runner, attention/participation, receipts, controller presence, shared skill;
no scheduler, provider SDK, remote transport, or changes to the pilot's live daemon.
Invariants: retain unacknowledged receipts; use the same selection in wait and
prompt; include replies to outgoing requests; distinguish subscription from
participation; never infer acknowledgment from a reply or successful child exit.
Controller updates require a live run token and must not resurrect explicit leave.
Changes: protocol/schema v2 with transactional v1 migration; involved/all selection;
4 KB routine packets; explicit bootstrap; atomic store/agent-qualified batch ack;
30-second controller heartbeat, 120-second lease, bounded child runtime.
Evidence required: routing/migration/receipt/fencing Rust regressions, deterministic
IPC runner tests including empty startup, backlog stall, wait selection and busy/idle
presence; cargo fmt, strict Clippy, cargo test, release build, integration.py and
check_sql.py. No paid model calls or claims about provider token consumption.
Risks: old daemons require a coordinated restart; never restart another session's
daemon automatically. Existing customized host skills must not be overwritten.
Completed: all five fixes implemented; 59 Rust tests, 28 IPC checks (27 together
plus one isolated compatibility check), strict Clippy/formatting, release build,
and 22 SQL checks pass. See `VALIDATION.md` for evidence and qualification limits.

Fray should make a small team of local agents act like collaborators: ask a
specific peer, preserve an answer, surface a disagreement, hand work over, and
rejoin with enough current context to contribute. Transport alone is insufficient;
the receiving host must supply the information to a model at a useful boundary.

Mote owns tickets, epics, dependencies, and work reservations. Fray owns live
conversations, questions, current decisions, and attention. Refer to Mote IDs;
do not synchronize competing task states. One or two stewards can route questions
and maintain shared context, using the same API as everyone else. Their absence
must not block communication or already authorized work.

## Assessment of the supplied prototype

The five-dependency Rust/SQLite design is worth keeping. Transactional fan-out,
revision checks, lease fences, exact acknowledgment sequences, bounded briefings,
and replayable socket delivery address concrete failure modes. No broker, async
runtime, provider SDK, model call, or mandatory manager belongs in the core.

The task-card vocabulary dominates the interface, while direct conversation is
cumbersome. More seriously, fan-out excludes the publishing actor without
recording participation: an off-topic contributor may miss later answers. Rejoin
seeding also ignores earlier participation and existing unassigned work outside
the agent's scope. These violate the collaboration story despite the happy-path
tests. The source arrived uncompiled and unformatted, without a lockfile.

The host boundary remains explicit: a socket write does not wake an arbitrary
interactive model. The generic process runner can wake idle command-line agents;
Claude hooks supply context at lifecycle boundaries. Provider-specific in-turn
delivery needs a tested adapter. A DeepSeek-backed host can use the same CLI and
stdin runner contract; Fray itself is not a model client.

## First implementation contract

- Scope: one Unix workstation, one binary, local SQLite, multiple independent
  agent processes. Keep the existing schema and five direct dependencies.
- Add `send AGENT BODY`, `reply ID BODY`, and `thread ID`, reusing cards and
  annotations. `send --ask` creates an open question; `--ref mote:ID` stores a
  queryable reference. Directed sends route through an agent-specific topic;
  all content remains public and steward-visible.
- Record contributors durably without acknowledging unseen peer updates. Rejoin
  must recover relevant current conversation heads. Existing unassigned work
  must be discoverable by newcomers.
- Retain precise receipts, current summaries, history, bounded context, and
  idempotent mutation retries. Replies are evidence, not automatic resolution.
- Make the source buildable and tidy, lock dependencies, and run formatting,
  strict Clippy, Rust tests, IPC/crash/runner tests, and the SQL checker.
- Demonstrate a manager and differently scoped peers exchanging a question,
  answer, objection, and handoff across a restart. Measure local release latency
  without interpreting socket timings as model response timings.
- Embed one collaboration skill for Codex and Claude, with worker/steward guidance
  and a project-local installer that preserves customized skills and settings.

Actual Codex/Claude/DeepSeek model cooperation, Linux qualification, retention,
backup commands, and native host steering remain separate acceptance work.
This implementation should earn a reliable local foundation, not a premature
claim of being the premier collaboration substrate.

The local implementation preserves the multi-workstation direction through an
explicit protocol version, store-qualified IDs/cursors, and separate transport
and state transitions. See architecture section 9 for the network extension
contract. No remote access or distributed database has been added.

The missed-reply and returning-participant regression tests were executed against
the original archive source: both failed with an empty inbox. They pass with the
participation fix. Addressed messages and existing conversations now queue while
recipients are absent, preserving final outcomes as well as active heads.

Implementation anchors: `src/store.rs` (`emit`, `mutate`, `RELEVANT`),
`src/main.rs` (conversation commands), `src/driver.rs` (runner), `src/server.rs` (wait
notifications and protocol envelopes), and `tests/collaboration.rs` plus
`scripts/integration.py`. Exact acceptance commands/results are in `VALIDATION.md`.
