# Validation

## Durable waits and host-neutral attention (2026-09-23)

Local macOS gates for indefinite waits, precise exit status, shared kind/priority
filters, bounded wake packets, reconnects and listener ownership:

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo check --locked --all-targets` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo test --locked --all-targets` | 82 passed: 8 attention, 9 client, 14 collaboration, 4 framing, 5 pilot, 9 skill, 33 state |
| `cargo build --locked` | Passed |
| `python3 -W error::ResourceWarning scripts/integration.py target/debug/fray` | 32 passed |
| `python3 -W error::ResourceWarning scripts/attention_integration.py target/debug/fray` | 19 passed, no resource warnings after closing the sleep-test SQLite fixture |
| `python3 scripts/check_sql.py` | 51 static statements prepared; 22 checks passed |
| `claude plugin validate integrations/claude-plugin` | Passed without warnings |
| Python syntax for adapters and host harness | Passed |

The new IPC cases exercise silent heartbeats, unrelated traffic, burst settlement,
urgent bypass, old priority backlog, daemon crash/reconnect, replacement database
rejection, explicit leave, duplicate consumers, sleep-expired leases, immediate
replacement after a killed watcher, and unexpected stream input. Wait checks
exercise `none`, exits 0/3/4, exact ACK resume, hidden receipts, identical filter
results across inbox/wait/watch, and 140 cancelled indefinite connections without
exhausting the 128-client cap. The final client change also maps a daemon
disconnect during handshake to unavailable; all 82 Rust tests and both wait IPC
cases passed after that change, and Clippy/build/format checks passed. Rust cases include 379 budget-boundary combinations,
Unicode/escaping, urgent content preservation, and durable objection origins.
The SQL script does not cover dynamic filter SQL or the attention module; Rust
and IPC tests execute those paths against real SQLite stores.

Claude independently reviewed a scratch copy through the shared Fray board.
Objection #3 reproduced packet-budget failure and urgent-content shrinkage; both
were fixed and the reviewer independently checked body lengths 1..1499 at two
budgets. Objection #4 reproduced the stale listener after SIGKILL. The reviewer
then reran the supplied 16-case IPC suite to verify that fix; this was a peer run
of our regression, not a second independent kill/restart reproducer. Both review
cards were resolved by the reviewer. The final wait/filter delta received a
separate review request in thread #2.

Real hosts were exercised only on isolated synthetic boards:

| Host path | Observed outcome |
| --- | --- |
| Codex CLI 0.156.1, one managed `drive`/`exec` turn | Passed: received and explicitly acknowledged fixture card 1 through sequence 1; child exited 0; owned processes stopped |
| Existing Claude session, native Monitor consuming `watch --attention` | Passed: armed listener observed before publication, peer reported idle wake, read full thread, replied, ACKed precisely through sequence 1; stopped listener verified independently; owned fixture daemon stopped |
| New Claude CLI 2.1.280 session with optional plugin | Unqualified: isolated PTY attempts remained at workspace trust before any idle/wake test; their processes were stopped |

The native Monitor notification display truncated the JSON line. Fray emitted a
complete bounded packet, but the peer needed `thread --bodies` for full context.
Native Monitor success does not establish automatic plugin loading, one-shot
asyncRewake behavior in a real host, Codex interactive-session injection, or other
providers. The asyncRewake adapter's exit mapping is covered by synthetic IPC.

Raw gate logs and exact-source manifest are in `/tmp/fray-attention-checks/`:
`final-rust-handshake.log`, `final-request-ipc-cleanup.log`, `final-legacy-ipc.log`,
`final-clippy.log`, `final-wait-handshake-ipc.log`, and `source-sha256.txt`.
The final manifest SHA-256 is
`78d7a2e17f2844b47cff7d96e86517b96e3ed2f8d748e0d4790def2ea2497655`. Host receipts are recorded in
`/tmp/fray-host-codex-attention-v3/report.json` and
`/tmp/fray-attention-checks/claude-monitor-acceptance.json`; Claude's report is also
on shared thread #2 at sequence 33. Earlier failed host attempts were retained.
No shared daemon was restarted and no global binary/plugin was installed. This
source copy has no Git metadata; these results describe local source, not a
commit, published release or hosted CI run. Linux remains unqualified locally.

## Collaboration feedback follow-up (2026-09-22)

Local macOS checks for readable threads, longer messages, inbox filters, workspace
discovery, and optional bundled skills:

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo check --locked --all-targets` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo test --locked --all-targets` | 72 passed: 7 client, 14 collaboration, 4 framing, 5 pilot, 9 skill, 33 state |
| `cargo build --locked` | Passed; local `target/debug/fray` |
| `python3 -W error::ResourceWarning scripts/integration.py target/debug/fray` | 32 passed |
| `python3 scripts/check_sql.py` | 50 static statements prepared; 22 checks passed |
| Skill creator `quick_validate.py` | Core, seam and review skills passed |

The new tests check exact full-body persistence through daemon restart, history
search, single-event idempotent retry, UTF-8 byte limits including acceptance at
8,000 bytes, pre-mutation capability rejection, ordered multiline thread output
and pagination, filtering before counts/pagination without consuming receipts,
read-only board discovery, ancestor workspace-home selection, and bundle installer
preflight/idempotence. Focused collaboration and skill tests were rerun after
strengthening their byte-boundary and CLI assertions. The full IPC suite retains
the existing idle-driver wakeup and busy/idle controller tests.

Raw logs and status sidecars: `/private/tmp/fray-feedback-bodies-ipc.log`,
`/private/tmp/fray-feedback-clippy.log`, `/private/tmp/fray-feedback-sql.log`.
No model host was invoked; IPC children are deterministic fixtures. No live pilot
daemon, global binary, or installed project skill was changed. No schema or
dependency migration is needed. Old protocol-2 daemons need a coordinated upgrade
for `long_messages` and `inbox_filters`; unsupported requests fail before sending.
Queued locks, typed review lifecycle/staleness, Mote mirroring and interactive
mid-turn wakeups remain unimplemented; see [assessment](REVIEW.md).

The results below are historical and describe earlier source/installations.

Local validation performed on 2026-09-21 using macOS 14.3 arm64, Rust/Cargo 1.91.1,
and Python 3.14.7. The generated Cargo.lock pins 61 packages; the binary still
has five direct dependencies. This is a locally tested development version,
not an actual-host or cross-platform qualified release.

## v0.2.1 client/daemon compatibility fix

The updated pilot exposed a gap in 0.2.0: only drive checked the daemon protocol;
inbox/wait sent unsupported selection fields to a live v1 daemon, and start reported
success. Compatibility now lives in the shared client, on the same connection as
the requested operation. Every reconnect rechecks it; no selection fields are dropped.
Start rejects an incompatible live daemon without creating replacement artifacts.
Ping and explicit stop remain usable for recovery. Package-version differences
are allowed when both sides use protocol 2; no schema or dependency changes.

Final local checks: formatting, strict Clippy, all 64 Rust tests, all 29 IPC tests,
49 SQL statements/22 SQL checks, and the release build passed. New tests cover
old/new/missing/malformed protocol metadata, same-protocol package skew, exact
request preservation on the checked socket, no cached compatibility decision,
ping/shutdown recovery, 11 incompatible CLI entry paths, quoted recovery homes,
and rejection after a watch reconnect. No unsupported operational request reaches
the mock legacy daemon. Real v2 daemon tests cover the existing collaboration flows.

The first test fixture runs exposed inherited nonblocking sockets on macOS and
the /tmp-to-/private/tmp path alias. Fixtures now explicitly select blocking I/O
and use canonical paths; protocol/request assertions remain unchanged in intent.
Raw results and exit-status sidecars: `/private/tmp/fray-compat-final-*.log`.
Initial fixture failures remain in `/private/tmp/fray-compat-client-01.log` and
`/private/tmp/fray-compat-ipc-focused.log`. No paid model or live pilot daemon was used.

Release: 3,226,112 bytes, SHA-256
`8bac759851d3ee8a4e475bb95989eb2946d3ef6b2c4f38f01ad9ebab2bdcd2e9`.
Installed `/Users/bbuchsbaum/.cargo/bin/fray` reports 0.2.1 and is byte-identical
to that tested release. Existing project daemons and skill copies were untouched.

## v0.2.0 pilot fixes: historical checks

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo test --locked --all-targets` | 59 passed: 33 state, 11 collaboration, 5 pilot, 4 framing, 6 skill |
| `cargo build --locked --release` | Passed; 3,226,032-byte binary; five direct dependencies |
| `python3 -W error::ResourceWarning scripts/integration.py target/release/fray` | 27 passed, plus the subsequently added old-daemon compatibility test passed separately (28 total) |
| `python3 scripts/check_sql.py` | 49 static SQL statements prepared; 22 checks passed |

Pilot regressions cover no model invocation on empty startup, explicit bootstrap,
4,000-byte routine prompts without available-work suggestions, narrower steward
selection, outgoing-request reply wakeup, ignored backlog stopping after one turn,
partial progress, atomic batch ack and wrong-store rollback, own-reply head/receipt
differences, and retained receipts without accidental permanent following.

Synthetic v1-schema migration preserves store identity, pending/ack versions and
contributor routing, and does not repeat its backfill on reopen. Fenced controller
tests use a deterministic clock for expiry, duplicate ownership, replacement and
leave. IPC tests also wait through a real 30-second heartbeat interval with busy
and idle controllers; idle presence requires no model invocation. Child failure,
timeout without stdin consumption, prompt-budget failure, and hook suppression
inside drive are covered. A mock v1 daemon receives only ping: no shutdown or join.

The new retained-scope test initially caught SQL NULL propagation through a negated
routing predicate. The implementation now uses `coalesce` and the unchanged test
passes. No failures were suppressed or acceptance thresholds relaxed.

v0.2.0 release SHA-256:
`1a42d871ffb76cd7a12b57f7965b8aef564d22e35f1e0631a34b68d51a5ae866`.
Raw evidence and exit-status sidecars are `/private/tmp/fray-fixes-final-*.log`
and `/private/tmp/fray-fixes-compatibility.log`. The earlier failing regression
is preserved in `/private/tmp/fray-fixes-check-02.log`.
At that handoff, `/Users/bbuchsbaum/.cargo/bin/fray` reported 0.2.0 and was
byte-identical to the tested release. Its printed skill matched the shared source.
The current SQL-only report is [sql-validation.json](sql-validation.json).

All model hosts were deterministic Python stand-ins. No paid model was launched,
no live pilot state/daemon was changed, and provider token savings were not measured.
The local CLI upgrade does not update existing project skill copies; review/merge
the new embedded `fray skill` into those copies without overwriting custom content.
Coordinate a daemon stop/start to migrate to schema/protocol v2. Back up the stopped
project home before upgrading; old Fray rejects the upgraded schema.

## v0.1.0 baseline checks (historical)

The following results and latency measurements predate the pilot fixes; the
benchmark was not rerun for v0.2.0 and does not qualify its performance.

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo check --all-targets` | Initial source build passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Final source passed |
| `cargo test --locked --all-targets` | 54 passed: 33 state, 11 collaboration, 4 framing, 6 skill |
| `cargo build --locked --release` | Passed; 3,176,048-byte binary |
| `python3 -W error::ResourceWarning scripts/integration.py target/release/fray` | 15 passed |
| `python3 scripts/check_sql.py` | 35 SQL statements prepared; 21 checks passed |
| `python3 scripts/bench.py target/release/fray --n 500` | 500/500 events delivered |

The integration tests cover replay/live subscription, competing claims,
crash/restart persistence, idempotent retry, malformed requests, byte budgets,
synthetic Claude hooks, idle runner wakeup, and the ignored-receipt stop condition.
A two-steward scenario exchanges a question, answer, objection, evidence, and
handoff among differently scoped stand-in peers across a daemon restart.

Skill tests exercise both host paths, repeat installation, preservation of custom
skills/settings, refusal to follow configuration symlinks, and CLI installation
without a daemon or agent identity. After the last skill prose edit, all six skill
tests were rerun; the rebuilt release passed all 15 integration tests again.

The off-topic reply and returning-participant regression tests were also run
against the original archive source: both failed, returning zero pending items.
Both pass with the fix. Other added tests cover closed outcomes for absent
participants, queued direct messages, newcomer discovery, long names/Unicode,
idempotency, and preserving unseen updates when an agent contributes.

The first Clippy run exposed one pre-existing `map_or` simplification; it was fixed.
The first integration run passed but exposed an unclosed Python socket during
startup retries; both harnesses now close failed connections, and the integration
suite was rerun without the warning. No tests were removed or thresholds relaxed.

## v0.1.0 local benchmark (historical)

WAL and `synchronous=FULL`; one publisher, one watcher, a fresh database, 500 notes.
This includes Python framing and scheduling. It is not a measurement of model
attention, realistic project history, or many-agent throughput. The final run
overlapped the local integration suite for its first second; other background
load was not controlled. These are observations, not a latency guarantee.

| Measurement | Median | p95 |
| --- | ---: | ---: |
| Persistent publish round trip | 3.93 ms | 5.00 ms |
| Publish start to subscriber read | 4.11 ms | 5.15 ms |
| Separate CLI process ping | 5.26 ms | 5.58 ms |

Observed serial throughput: 246 publishes/second. Full distributions and environment
are in [benchmark-macos.json](benchmark-macos.json). The SQL-only report has since
been refreshed for v0.2.0; see the current checks above.

v0.1.0 release binary SHA-256:
`ae08a8916bba9f11f700d44772f67159bcf1e9e50ecd45796bd2e5c26069e015`.

Original zip SHA-256:
`10cae71525cb386c44eedc73171fd7d9282611f9bf56a298836c177dcebb1e9d`.

The original zip is retained unchanged. Raw local check logs and exit-status
sidecars are in `/private/tmp/fray-review.mnioKe/`; this is a temporary location,
not a durable release artifact.

## Reproduce

```sh
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
python3 -W error::ResourceWarning scripts/integration.py target/release/fray
python3 scripts/check_sql.py
python3 scripts/bench.py target/release/fray --n 500
```

This session used `CARGO_HOME=/private/tmp/fray-cargo-cache` because the sandbox
could read but not write the ordinary Cargo cache. A normal writable Cargo home
is sufficient. The CI workflow runs formatting, Clippy, Rust, IPC, and SQL checks
on Linux and macOS; no hosted CI run is claimed.

## Remaining acceptance

- Broader real-host coverage beyond the bounded Codex and Claude passes above:
  busy-session delivery, automatic plugin loading, and a specified DeepSeek-backed
  host. Stand-in peers are not model tests.
- Linux execution and hosted CI.
- Many subscribers, slow readers, long-lived projects, bounded retention and backup
  operations. Identity collisions outside `drive`, process-tree supervision and
  abrupt-death recovery beyond the controller's stale-lease indication.
- Any remote transport: authentication and authorization are deliberately absent
  from the trusted local socket protocol.

The earlier installed-Codex check confirmed the `codex exec -` stdin contract. Claude hook
output was checked against the [current primary documentation](https://code.claude.com/docs/en/hooks#add-context-for-claude)
and synthetic payloads. Those earlier checks alone did not establish real model
cooperation; the bounded real-host results are recorded above.
