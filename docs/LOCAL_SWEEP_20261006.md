# Local Mote sweep, 2026-10-06

Base: `b2cd1b9a187a5f9be14f66924e164732c3e3d979`. Work is isolated on
`codex/outstanding-motes`; unrelated root and existing K2 worktree changes
are preserved. Paid Agent Mail installation/comparison remains deferred by
owner direction. No remote push, release, live board prune, or live paid host
trial is part of this sweep. The owner's later upstream escalation request was
completed as [Mote #18](https://github.com/bbuchsbaum/mote/issues/18); the posted
body was read back and linked to the blocked authority tickets.

## Changes and evidence

- Snapshot capture hashes while copying, using streaming safe Rust SHA-256
  checked against FIPS vectors and `shasum`, including block/padding/chunk
  boundaries. Every copied payload file is still flushed before publication,
  with four bounded flush workers. A 500-file create-and-verify measured
  595.958208 ms; all 502 digests agreed with `shasum`. This is one local
  specimen, not a cross-tool benchmark.
- Review headers report the assigned reviewer's latest verdict; advisory peer
  approvals cannot mask that reviewer's objection. Terminal reviews refuse
  new verdicts without changing history.
- Metrics read a separate read-only WAL snapshot outside the publisher mutex.
  First routing timestamps keep old content from inflating a newly assigned
  recipient's attention age; legacy unknown routing times remain explicit.
- Objection closure requires the objector or owner. Closure and owner overrides
  create durable addressed notices, including when the source is muted or a
  recipient has left. Bounded attention surfaces objection IDs and overrides.
- Ask/lapse origin remains stable across later annotations. Mote request
  completion comes from Mote; local replies cannot erase it. Escalation caps
  do not settle an undelivered follow-up.
- `peek`, owner HTML, and Markdown export read without registering presence,
  exposure, or ACKs. Exports keep complete selected histories and stable IDs,
  escape untrusted HTML, and refuse differing output or leaf symlinks.
- Role routing uses active real Mote assignments plus Fray liveness, and refuses
  before queuing when the Mote session has ended. Non-Mote boards use declared
  Fray roles. Routing and receipt are not ownership.
- Online backup and fresh restore compare store identity, cards, events,
  deliveries, presentation receipts, and Mote cursors. Backup files are private
  and validated for integrity, required tables and foreign keys.
- Offline opt-in retention archives and verifies complete history plus a
  recoverable SQLite copy before replacing old terminal payloads with metric
  projections. Protected references/receipts are retained; old replay cursors
  fail explicitly. See [retention](RETENTION.md).
- Ended host-session peer exposure cursors are pruned without removing live
  peers, card history, or durable receipts.
- Keepalive K2 integrates terminal busy hooks, re-fork on terminal progression,
  while-away context, first-fork token baseline, session-scoped waits, pinned
  Team model/budget, and read-only doctor recovery reporting. Synthetic host
  integration passed 12/12; real host and sandbox qualification remains open.
  Doctor skips keepalive probes when an older daemon does not advertise support;
  the old-protocol fixture also exercises subsequent monitor and rewake calls.

The core still has five direct dependencies and no unsafe code. The existing
`rusqlite` dependency gains its online-backup feature; no new dependency is added.

## Qualification boundaries

The fresh baseline passed 385 tests over 39 targets, including installed Mote
integration. Focused checks accompany each changed contract. Required integrated
checks and independent exact-SHA review are recorded in [Validation](VALIDATION.md).

The spawn regression ran 36 separate test processes against an immutable build,
with one test thread per process: 36/36 runs, 1,368 checks, no spawn ENOENT.
The default-threading 36-process overload also ran: 3/36 processes passed,
with Mote timeouts and consequent attention assertions failing. No spawn ENOENT
occurred. Both raw results are retained; the controlled run does not erase the
unsupported overload result or relax production timeouts.

Actual owned-daemon SIGKILL/restart verifies pending delivery survives, a stale
receipt cannot consume a newer update, a retried request key creates no duplicate
event, and database integrity/store identity persist. Bounded in-flight daemon
kill races and a client send-completion handshake verify atomic retry outcomes;
reconnected wait/watch observe later updates. This does not prove every SQLite
instruction boundary, power-loss recovery, or live-host controller recovery.

Archive publication syncs file contents and directory entries before compaction
commit; an injected publication failure leaves history, cursor floor and audit
unchanged. Snapshot payload flushes do not establish whole-bundle power-loss
durability: its manifest and publication directory were not qualified.

The soak gate is 30 agents and four fully drained watches, 60 seconds warmup,
then 7,200 seconds measured mixed send/inbox/ACK/wait/watch. Bounds declared
before launch: FD count 256, RSS 256 MiB, WAL 64 MiB, publish p95 100 ms;
no delivery mismatch, duplicate, timeout or escaped SQLITE_BUSY. Short smokes
passed operational checks and explicitly report `qualified:false`. The full
default gate then passed on frozen `9be4977`: 106,343 total send cycles,
7,201 measured resource samples, maximum FD 79, RSS 31.52 MiB, WAL 5.70 MiB,
and publish p95 24.104 ms. Recorded error counters were zero. Raw UTC gaps
are retained and disclosed in [Validation](VALIDATION.md); this qualifies the
local monotonic-duration workload, not uninterrupted wall-clock or native-host
service. Later diagnostic changes have independent checks; the result is not
whole-binary performance qualification of the later implementation commit.

HTML escaping and structure checks passed; the product browser was unavailable,
so rendered visual qualification was not performed. The temporary preview
server was closed and the browser-process audit found none.

Authoritative review/landing and strict dispatch/handoff remain blocked on
[upstream authority capabilities](MOTE_CAPABILITY_GAPS.md). Their acceptance
contracts are preserved. Keepalive is not announced until its real sandbox
qualification passes. The paid head-to-head comparison and parked MCP shim
remain deferred; therefore the broader epic cannot be closed.
