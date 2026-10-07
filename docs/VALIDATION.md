# Validation

## Local Mote authority workflows (2026-10-06)

Mote prerequisites are independently approved local commit
`a86803de31e890ac6e4331c9961705cabcde2fcb`: confirmed-journal I/O failures and
final Git drift return nonzero recovery receipts, and both claim/begin activate
stable ordering before acquisition. Mote passed 423 active Rust tests with two
pre-existing ignored, formatting, Clippy and rustdoc warnings denied.

Fray implements explicit immutable candidate requests/verdicts/successors,
author-only local integration-evidence attention, fenced nonempty local
fast-forward landing, eligible idle-peer offers, explicit Mote-confirmed accept,
disappearance reconciliation, and recoverable carrier close/adopt. Exact client
journals preserve failed requests/receipts; historical retries do not renew
claims, reservations or approvals. See the [revision 6 contract](design/mote-adapter.md).

The final source passed 452 Rust tests over 50 targets, formatting, locked
all-target checking, Clippy with warnings denied, and build. The independent SQL
scanner prepared 237 statements against both schemas and passed 22 invariants.
Always-run synthetic public-CLI courts cover mixed holder/token reads, bounded
churn refusal, exact confirmation recovery after renewal, unsafe self-carrier
refusal, admitted history suppression, earlier-spelled anchors, future raw ids
with due reservation warnings and timeout cursor preservation.

Real disposable Mote/Git/Fray courts passed eleven review/landing and six
dispatch/handoff cases. These include nonzero admitted-review recovery with no
premature Fray verdict, object/successor/approve/land, revoked authorization,
base advancement without reviewer wake, actual client SIGKILL after Mote archive
with historical recovery/drift refusal, cross-session committed-response recovery,
invalid adoption TTL refusal, concurrent accepts with one actual
claim, interrupted handoff/close/adopt, competing adoption, carrier expiry and
actual bounded claim-expiry requeue. During a paused Mote landing an unrelated
Fray RPC returned promptly; the exact duration is retained in each court log. This is a bounded observation, not a latency SLA.

Required Python suites cover native-wake transport fixtures (23), ordinary IPC
(38), attention (28) and descriptor/cancellation reliability (7). They do not
execute paid models or native keepalive qualification. Final logs, source hashes,
binary hashes and the local Mote prerequisite patch are retained in the
[workflow evidence](evidence/fray-mote-workflows-20261006/manifest.json).

No push, global installation, shared daemon restart, original-store authority
activation, hosted CI or paid trial is claimed. The prior two-hour soak remains
bound to its original source/binary; it does not qualify this entire new binary.
[Mote #18](https://github.com/bbuchsbaum/mote/issues/18) tracks upstream publication
of the needed primitives/fixes. Real native-host trials, the paid comparison,
parked MCP shim and their encompassing epic remain outside local completion.

## Structured Mote failure receipts (2026-10-06)

Fray implementation `a20432b21ef2d75274077fd54bd3a630d6015ee8` preserves a
complete structured command receipt when Mote exits unsuccessfully or is
signalled. Journal paths, exact old/new/current Git OIDs, uncertainty flags,
and replacement holder/token remain available to the caller. The reported
exit is retained; this receipt does not confirm a successful mutation. Usage
errors, legacy rejections, successful transport results and preflight behavior
retain their existing classification.

All 22 Mote transport tests passed, including the bound subprocess's real
exit-2 receipt and Git-updated/unknown/current-holder cases. Formatting,
locked all-target checking, Clippy with warnings denied, all 428 Rust tests
over 46 targets, and build passed. [Raw logs and source provenance](evidence/fray-authority-receipts-20261006/manifest.json)
are retained. This step does not complete the authoritative review, landing or
handoff workflows. Their historical upstream gates are documented in
[Mote integration gates](MOTE_CAPABILITY_GAPS.md). Timeouts still report an
unconfirmed failure without recovering buffered output; consumer recovery
must use the journaled request and authoritative readback.

## Local Mote sweep (2026-10-06)

The [sweep evidence](LOCAL_SWEEP_20261006.md) separates local implementation,
focused tests, crash/restore checks, stress limits, and outstanding qualification.
Paid comparison remained deferred at that stage. Mote authority gaps (now
addressed locally above) and real keepalive sandbox qualification were open; no broad epic completion or release is claimed.

The implementation landed on local `main` at
`bb6fb6f01384f3ba6b512b2463e43702a3d16d2a`, with independent exact-commit approvals
from `storage_review` and `keepalive_snapshot_review`. No remote push or shared
daemon restart was performed. Formatting, locked all-target checking, Clippy
with warnings denied, and all 426 Rust tests over 46 targets passed, including
the installed Mote integration. Host-neutral IPC passed 38 tests, attention 28,
descriptor/cancellation reliability 7, and drive/keepalive 24. Native-wake's
23 Python tests also passed; these are transport fixtures, not paid host trials.
The SQL scanner passed its 193 prepared statements and 22 invariant checks.

Raw logs, environment versions, and their provenance are retained in
[local sweep evidence](evidence/local-sweep-20261006/manifest.json). The initial
sandboxed terminal job-control test failed because `stty` was denied terminal
ioctl access; the owned-terminal rerun passed all 23 lifecycle cases. An
old-daemon attention failure revealed an unsupported doctor probe, now fixed
and covered by the passing old-protocol monitor/rewake fixture.

Reproduce the local reliability gates with synthetic data:

```sh
cargo test --locked --test crash_recovery --test recovery
cargo build --locked
python3 -W error::ResourceWarning scripts/drive_integration.py target/debug/fray
cargo build --locked --release
python3 scripts/soak.py target/release/fray --out /tmp/fray-soak-evidence
```

The soak defaults are 30 persistent peers, four fully drained watches, 60 seconds
warmup and 7,200 seconds measured traffic. Every send's exact receipt is checked
before acknowledgement and every watch's event sequence is checked for gaps
and duplicates. Predeclared maxima are 256 descriptors, 256 MiB RSS, 64 MiB WAL,
and 100 ms publish p95, with no delivery error, timeout, duplicate or escaped
`SQLITE_BUSY`. Short runs explicitly remain unqualified. The frozen artifact's
identity is retained in [artifact.json](evidence/local-sweep-20261006/artifact.json).
The default gate completed with `success:true` and `qualified:true`: 106,343
send/inbox/exact-receipt-ACK/wait cycles across warmup and measurement, four
sequence-checked watches, and 7,201 measured resource samples. Recorded delivery
failures, duplicates, timeouts and escaped `SQLITE_BUSY` were all zero.

| Measured quantity | Observed | Declared limit |
| --- | ---: | ---: |
| Maximum descriptor count | 79 | 256 |
| Maximum RSS | 31.52 MiB | 256 MiB |
| Maximum WAL size | 5.70 MiB | 64 MiB |
| Publish p95 | 24.104 ms | 100 ms |

Publish p50 was 8.506 ms and the maximum was 422.304 ms; the predeclared latency
gate bounds p95. The [report](evidence/local-sweep-20261006/soak-report.json),
[compressed raw samples](evidence/local-sweep-20261006/soak-samples.jsonl.gz),
and [sample analysis](evidence/local-sweep-20261006/soak-analysis.json) are retained
with hashes and the logger's zero exit status. The owned daemon, harness and
logger exited; the temporary home was removed, and the job-bound idle-sleep
inhibitor exited.

The duration gate uses macOS `mach_absolute_time()` through Python's monotonic
clock. Raw UTC samples span 9,910.671 seconds and contain gaps of approximately
421, 1,492 and 741 seconds; their cause is not established by samples alone.
The pass covers the specified 7,200 measured monotonic seconds after warmup and
sampled resource/delivery correctness. It does not establish uninterrupted
wall-clock service, wall-clock response deadlines, or native-host reliability.
To replay the recorded resource gate's source, build the full commit named in
`artifact.json`; the current implementation's diagnostic corrections have
separate checks described below.

The SIGKILL gates also verify a consumed delivery's persisted `ack_seq` and
absence from the recovered inbox. The managed-runner crash fixture reaps its
runner, verifies its owned process group is empty, and observes safe refusal of
a replacement while the original 120-second controller lease is live.
`--release-orphan` does not revoke that lease. After verifying the original
runner and group have stopped, the operator explicitly issues
`fray --as NAME leave`, then starts a fresh drive run. The fixture verifies the
original exact receipt tuple survives, one replacement child receives it, and
only its acknowledgement consumes it. This is operator-assisted local recovery;
automatic lease-expiry and native-host recovery remain unqualified. The focused
recovery test completed in 30.520 seconds; cleanup time was not measured separately.

The lifecycle suite requires a debug build: its sandbox-refusal and deliberately
tiny packet-budget specimens use debug-only test overrides. An optimized-binary
run passed 22/24 cases and failed those two specimens because release builds
intentionally ignore those overrides. The clean debug rerun passed all 24 cases.
That failed optimized log is retained; it does not
qualify either release sandbox behavior or native hosts. The resource soak uses
the separately frozen optimized binary.

Two unpaid release specimens use a custom real macOS seatbelt profile that
denies execution of `/usr/bin/sandbox-exec`. The keepalive guard refuses the
request with `sandboxed`; doctor reports `daemon_sandboxed:true` and owner
recovery instructions. No native host request was made, and the owned daemons
and homes were cleaned up. The profile and raw outputs are retained. This
exercises release probe refusal, not actual Codex sandbox inheritance or general
nested-seatbelt behavior. It exposed recovery suggestions that dropped an
explicit selected home. The corrected formatter preserves `--home` for stop/start
and both home and identity for retry, quotes literal shell arguments, and names
the original host session and repository as the retry context.

The two-hour resource artifact remains the frozen `9be4977` optimized binary.
The later recovery-message correction does not change the send/inbox/ACK/wait/
watch operations measured by that workload; its own command and sandbox checks
are separate evidence.

The final all-target log (`fray-motes-final-checks-03.log`) and the affected
38 IPC, 28 attention, and 24 lifecycle checks were rerun after the recovery
formatter correction and passed. A focused test executes the suggested shell
commands through a harmless argument-recording stub from an unrelated working
directory, with no Fray environment defaults and a home containing shell
metacharacters. The separately frozen `bb6fb6f` release artifact also passed
the custom seatbelt doctor/start refusal specimen with identical selected-home
recovery instructions. Its hash and raw output are retained separately from
the resource-soak artifact.

The source-reviewed upstream authority requests were filed and read back as
[Mote #18](https://github.com/bbuchsbaum/mote/issues/18). The receipt and exact
issue body are retained. They do not establish executed race reproductions or
unblock the two strict authority contracts.

## No silent stalls (2026-10-01)

`docs/design/no-silent-stalls.md` slices R1 to R5, on branch
`claude/esc-followup` (main 63ae3a0 plus R5, R2, R4, R3 as approved at
a8dabdf, and the follow-up commits through the reviewed SHA). `CARGO_INCREMENTAL=0 cargo
test` on macOS 14.3 arm64: every test target passes, including those named
below. The Mote tests ran against the real `mote 0.1.0`; they skip
themselves when `mote` is not on `PATH`.

### R1: one reachability test

`tests/reachability.rs` (9 tests, in-memory store with explicit clocks) and
`tests/wake_waits.rs` (1 test, a real daemon) prove:

- a turn that ended 5 minutes ago is `present` and the send notice says
  "nothing armed to wake it"; an hour later it is `absent`;
- an armed native-monitor listener is `wakeable` until its declared expiry,
  and not after, although the transport is still connected;
- a listener filtered to one card, or with no declared activation, is
  `present`;
- a driven agent is `wakeable` while its controller waits and while a child
  runs;
- only an unfiltered wait in progress is `wakeable`. Each wait counts on its
  own: a concurrent filtered wait, or a second unfiltered wait that times
  out, leaves the first one counting, and the armed wait's client hanging up
  ends it at once;
- a question prefers a wakeable assignee over a present author, and the
  result says who was passed over and how to reroute; a send notice names
  who is wakeable;
- friction counts a present but unarmed addressee as unreachable.

Approved at 607a29b by independent review (fray card #26).

### R5: arming and lapses

`tests/arm.rs` (2 tests, a real daemon and the built binary) runs the Stop
hook for an unarmed agent with an ask addressed to it: the hook blocks, and
its reason begins with `FIRST: 1 open ask(s) are addressed to you and
nothing is armed` and contains `fray --as helper arm`. It then runs the
command `fray arm --minutes 5` prints, through `sh`, as a host monitor would,
and the agent becomes `wakeable` within 10 s; with it running, `brief` has no
lapse. `fray arm --host background-completion` prints `--once --activation
background-completion` and no `--reconnect`. The second test
(`a_stewards_text_brief_names_the_no_runner_state`) checks that a steward's
text `brief` says when no runner is ticking.

`a_lapse_is_told_apart_from_a_rearm_and_from_leaving` in
`tests/reachability.rs` steps one agent through `unarmed`, covered, `lapsed`
(declared expiry passed) and `rearm` (a one-shot listener ended); the rearm
hint names `--host background-completion`; an unfiltered wait covers the
agent; an agent that left is told nothing. Approved at 9935207 (fray card
#26).

### R2: Mote requests tracked by state

In `tests/mote_sync.rs`, against `mote 0.1.0`:

- `a_mote_request_is_carded_once_and_settles_when_answered_in_mote`: an open
  request reaches its addressee as one card; a second sync creates nothing;
  a note is not carded; when the addressee answers in Mote, the card is
  resolved; `fray send` to an actor only Mote knows names the `mote msg send`
  command;
- `one_malformed_mote_request_does_not_stop_the_rest` and
  `the_daemon_skips_an_uncardable_request_and_cards_the_rest`: a request
  with a NUL in its body is carded with the NUL replaced, and a request Fray
  cannot card (an over-long message id) is skipped and reported while the
  others land (#79);
- `board_agents_requests_are_read_before_mote_only_actors`: many Mote actors
  who never joined cannot crowd out a board agent's requests (#80).

Approved at b3bd95f (fray card #26).

### R4: deadlines on asks

`tests/deadlines.rs` (8 tests, in-memory store) proves:

- an ask with `respond_within_ms` of 30 minutes is not overdue at 29 and is
  at 31, in the asker's `brief` and in friction (`overdue: true`,
  `soft_deadline: false`);
- a bystander's reply does not clear it; the addressee's does;
- patching tags, kind, status (short of closing) or assignee, by the asker,
  the addressee or a bystander, neither removes the deadline nor hides the
  ask (#81); after reassignment, the previous addressee's answer no longer
  counts;
- only the asker can move the deadline; a new one runs from the reply;
- a deadline without `ask` is refused; an ask without one never appears in
  `brief` and is overdue in friction after 24 hours (`soft_deadline: true`);
- with no addressee other than the asker, anyone else's reply answers it;
- after a bystander patches the kind to note, the asker can still move the
  deadline (`the_asker_moves_the_deadline_even_after_a_kind_patch`).

A unit test in `src/main.rs` checks that `--respond-within` accepts `30m`,
`2h` and `30d` and rejects `0m`, `31d`, `30s`, `-1h`, `1.5h`, an overflowing
count and non-ASCII units without panicking (#82). Approved at a460080 (fray
card #26).

### R3: escalation of stuck requests

`tests/escalation.rs` (13 tests, in-memory store) proves:

- an unreachable ask creates nothing within the grace period and, after it,
  exactly one card for each of two present stewards, assigned to them and
  visible in their `involved` inbox, naming `fray patch ID` and saying
  nothing re-routes automatically; a second tick under another identity
  creates nothing; the asker gets none;
- a driven addressee, or one shown the ask by `inbox`, is not unreachable;
- an overdue ask escalates even after it was shown, and both escalations are
  resolved at the next tick after the addressee answers;
- escalation cards are never escalated themselves, hours later;
- a Mote request to an actor not on the board escalates, says that actor
  has not joined, and settles when `mote_requests_sync` reports it answered;
- a steward's `brief` says no runner is ticking until a tick happens; a
  non-steward's does not; after a tick, `stuck_requests` lists the request
  and reports the board as ticking; a steward's text `brief` carries the
  note too (`a_stewards_text_brief_names_the_no_runner_state`, in
  `tests/arm.rs`);
- an addressee's reply ends "unreachable" even if nothing recorded showing it
  (`a_reply_from_the_addressee_ends_unreachable`);
- `escalation` (any case) cannot join;
- re-routing to another addressee who cannot be woken escalates again, and a
  steward who closed their card while the request is still stuck gets
  nothing before an hour has passed, then one reminder (#83);
- patching an ask's kind to `note` does not hide it from unreachable
  escalation (#86);
- 15 stuck asks to two stewards (30 cards due) produce 20 cards and 10
  deferred on the first tick, and the other 10 on the next;
- a Mote request whose addressee joins after it was first seen is listed
  once, as its card, not also as a request to an unknown actor;
- 1,001 unassigned asks without deadlines do not crowd a stuck ask out of
  the scan window.

`the_incident_reaches_a_present_steward_and_the_stuck_list` in
`tests/mote_sync.rs` replays the 2026-09-30 incident against `mote 0.1.0`:
alice sends helper a Mote request, and helper has nothing armed. With no
runner alive, `fray --json stuck` reports `ticking: false` and lists the
request from Mote directly. A steward then runs `watch --attention
--notification --selection involved`; with no other board activity, its own
background tick syncs Mote and escalates, and the steward's listener prints
`Stuck (unreachable): Mote request from alice` within 20 s. In
`an_answered_request_to_an_unjoined_actor_is_no_longer_stuck`, a Mote
request to an actor who never joined is stuck, and once it is answered in
Mote the next real sync re-reads it and `fray stuck` lists nothing (#84).

Not tested: owner review's terminal walk-through (it needs a TTY); a board
with several runners under real wall-clock time; and the brief's
readiness-details trimming at the minimum budget, which the shorter arm hint
no longer reaches in `tests/phase1.rs`. The R5 lapse counting only
unanswered asks is tested by `an_answered_ask_no_longer_counts_toward_the_lapse`. R3 was approved at
a8dabdf by independent review (fray card #26 @453).

## Git guard (epic child 4, 2026-09-30)

`fray guard install` adds `pre-commit` and `pre-push` hooks to the
repository's shared hooks directory. The bead proposed `fray hook git
--install`, but `fray hook` is the host-hook entry point, so the guard has its
own command.

The hooks check staged paths, and for a push, every path touched by each
pushed range:

- deletions are skipped;
- a new branch covers only the commits no remote ref has;
- an update covers the pushed commits, a merge's own changes, and whatever
  the push overwrites (so the paths of commits a force push drops); a remote
  tip this clone lacks is reported as unchecked.

Names are read NUL-separated, so non-ASCII and odd names match exactly.

Where Mote is adopted, another actor's Mote reservations decide; that takes
one `board --json` read per hook run. Otherwise Fray lanes decide.

The guard warns by default and blocks with `FRAY_GUARD=block` (exit status
10). It never blocks because a service is down or the check itself failed,
and a stale lane is reported but never refuses. Prior hooks are chained,
keeping their exit status and stdin.

`tests/guard.rs` has 10 tests, run in real git repositories:

- a foreign lane warns; blocking refuses; one's own lane is quiet;
- a failing prior hook stops the commit, and reinstalling does not wrap it
  twice;
- a push of a new branch plus an updated `main` names paths from both, the
  prior hook receives both ref lines, and a deletion checks nothing;
- a linked worktree shares the guard and the board;
- a stopped daemon produces "could not check" without blocking;
- against the real `mote 0.1.0`, another actor's reservation warns, and
  blocks on request, while one's own (named by `MOTE_ACTOR`) is quiet;
- a non-ASCII pushed name matches, and a force push over unfetched commits
  is checked;
- a failing check warns and only a refusal blocks; a clean commit is quiet;
- a stale lane reports without blocking;
- a merge's own edit, and a force push that drops another's commit, are
  refused under blocking.

Two independent reviews (fray #26 @377, @397) drove four fixes, #69, #70,
#75 and #76; approved at 92a723f (@407).

## Mote adapter: background sync (2026-09-30)

`fray watch --attention` and `fray drive` now sync Mote in the background,
paced for the whole board on the last sync any agent ran. Each runner starts
at a random point in the interval and sleeps 0.75 to 1.25 of it, so runners
started together do not stay in step (4 runners at 1 s for 10 s: 30 syncs
before, 10 after). `drive` starts the sync only after joining, and `watch
--once` never syncs.

The test `a_watching_agent_hears_mote_changes_without_anyone_running_sync`
runs against the real `mote 0.1.0`. Bob runs `watch --attention` with a 300 ms
interval, and alice hands him a claim; his watch prints the handoff notice
with no manual sync, in about 2 s. It waits for the first background sync
rather than sleeping, and accepts either delivery path (the handoff event, or
reconciliation when one sync straddles the handoff): 36 of 36 under 12-way
concurrency. Approved at bb2ddd7 (fray #26 @410).

The same test, run with `FRAY_MOTE_SYNC=off`, fails after its 20 s wait. So
the test does exercise the background path.

## Mote adapter slice 5c-b: candidates (2026-09-30)

`fray mote sync` reads the pending candidates once per sync (`mote
candidate list --phase pending`). Their cards come from current state, so
the listing both reports and reconciles.

- Named reviewers who have not reviewed are asked.
- The proposer and the authorizer hear status: landability, plus each
  blocking reason with its subject (for example "review_missing (dave)").
- Status is kept as the last state delivered per recipient, so a return to an
  earlier state is reported again.
- Candidates that land, are superseded (the card names the successor) or are
  abandoned are reported to everyone involved. That happens before the cursor
  moves past their events, and any truncation or failed read is noted.

Independent review of `05dfbc2` found a blocking case, reproduced against the
real `mote`: after a revoke and a re-authorization, the landable state reused
a key already delivered, so bob's newest card still said "blocked". That is
fixed by the last-state rule. The review also led to:

- reason subjects shown in summaries and counted in the state;
- terminal reports made safe against losing the cursor race;
- the notes.

Re-review of `12cedd8` verified that fix under about 400 concurrent syncs,
with no card that disagreed with the live state. It found that a client
talking to a daemon not yet restarted stopped all sync once a candidate
ended. Candidate reporting is now gated on the daemon's `mote_subjects`
capability, and when it is missing it is skipped with a note, while claims and
reservations continue. Final states are also sticky per recipient, and notes
accumulate instead of overwriting one another.

`tests/mote_sync.rs` has 28 tests:

- The unit tests cover:
  - review requests and re-asking after a policy amendment;
  - a return to an earlier state being reported again, while an unchanged
    state sends nothing;
  - subjects in reasons;
  - terminal phases.
- Against the real `mote 0.1.0`, in a git repository: a proposal (carol is
  asked, bob is told it is blocked), then carol blocks and re-approves, and
  alice authorizes, revokes and re-authorizes. Bob's newest card matches the
  candidate's live state at every step.

## Mote adapter slice 5c-a: claim reconciliation (2026-09-30)

Section 6 reconciliation, for claims, as state comparison. After ingesting
events, `fray mote sync` reads Mote's live board (`board --json`) and every
holder Fray has recorded (`mote_claims`). It compares the two holder by
holder, across every entity in either. Each difference is sent with the holder
Fray had, and the store applies it only if that is still what it records.
This compare-and-set means a concurrent sync is never overwritten; the next
sync rechecks any entry that lost the race.

- **Cards say what changed, not who changed it.** The new holder gets "you
  now hold E", and the previous holder gets "E is now held by X". They share
  the key `claimstate:E:<holder>:<lease_until>`.
- **A release or expiry the feed missed is recorded quietly.** Nobody is told
  anything that might be false.

The first design (`637458b`) rebuilt who moved what from `mote history`.
Independent review showed that was unsound, and all three objections were
reproduced against the real `mote`:

- A late op that Mote ordered before one Fray had already seen blocked
  reconciliation for good, while the sync still reported it as reconciled.
- A renewal looks like a handoff in history, so a receipt blamed the wrong
  agent.
- Reading the board and then history was not atomic, so a handoff in between
  was lost.

The state comparison removes all three causes: it never consults history or
op order, and the compare-and-set covers the gap between reading and writing.
Reconciliation also no longer makes one history call per entity. A daemon that
lacks the new operation degrades the sync to a note instead of failing it.

`tests/mote_sync.rs` has 20 tests. Against the real `mote 0.1.0`, with the
cursor forced past the events:

- a missed handoff followed by a renewal reaches the new holder, without
  blaming anyone;
- a missed third-party takeover reaches both agents;
- an entity alice never held tells her nothing;
- a missed release stays quiet and causes no false receipt later;
- repeated syncs deliver nothing twice.

Unit tests cover the compare-and-set race and the full holder listing.

## Model response through Fray, real hosts (2026-09-30)

`scripts/bench_model.py` (Mote bd-01M3RZ9BN4RYQRY56RFEPSTEJX) adds the
model-response column that `bench_wake.py` leaves out. The reply time is on
one monotonic clock; the host-start time (since the fix below) is from the
runner's wall-clock record. The owner approved a
bounded run. Each trial works like this:

1. A `fray drive --max-turns 1` agent waits with a real host as its child.
2. A peer sends it a p1 question asking for the reply `pong`.
3. The peer's own `fray wait --card ID` returns when the reply lands.

The reply time is measured on one monotonic clock from the moment the
publisher's `send` returned. There were 5 declared trials per host, failures
counted.

Setup: release build `9a39118`, macOS 14.3 arm64, Claude Code 2.1.285
(`claude -p`, with `Bash(fray:*)` pre-approved), Codex CLI 0.159.1 (`codex exec`,
workspace-write sandbox).

| Host | Answered | Publish to host started, p50 / p95 (superseded, see below) | Publish to reply, p50 / p95 / max |
|---|---|---|---|
| Claude Code | 5/5 | 0.04 s / 0.05 s | 4.98 s / 6.33 s / 6.33 s |
| Codex | 5/5 | 0.04 s / 0.05 s | 25.6 s / 34.7 s / 34.7 s |

Correction, from the review of `b7b4c89`: the "host started" column was
timed by a Python wrapper that stamped the time only after its own start-up,
about 30 ms of the 40 ms shown. The harness now reads the spawn time from
`fray drive`'s own run record, at millisecond resolution. With a stand-in
host that replies at once (`FRAY_BENCH_HOSTS_JSON`, 10 trials), Fray's part
measures:

- **publish to host spawned:** 2 ms (p50) and 3 ms (p95);
- **publish to reply:** 46 ms (p50) and 55 ms (p95), including the stand-in
  running `fray reply`.

So the model turn is effectively the entire end-to-end time. For an agent
that `drive` manages, a faster Fray cannot matter much; what matters is
waking the agent at all. Idle interactive sessions, which only hooks and
monitors reach, remain unmeasured here.

A reply counts when any annotation by the addressed agent lands on the card.
This measures latency, not whether the model obeyed the `pong` instruction.
Claude Code ran with `Bash(fray:*)` pre-approved; read-only tools such as
Read and Grep stay available. Codex ran in a workspace-write sandbox rooted
at the trial directory.

Pilot runs before the declared ones exposed a harness bug, now fixed: the
publisher waited with `--addressed-to-me`, which by design selects only
items assigned to you. A reply to your own question is assigned to the other
agent, so the filter hid it. The session log counts 16 model turns in total,
6 of them pilots; the artifact records only the 10 declared ones.

## Mote adapter slice 5b: Mote events into attention (2026-09-30)

`fray mote sync` and the `mote_ingest` operation implement the event path of
section 6 of `docs/design/mote-adapter.md`, for claims and reservations.
Candidates, and the reconciliation that covers skipped events, come in
slice 5c.

`tests/mote_sync.rs` has 15 tests:

- **Exactly once.** Each (store, state key, recipient) produces one card; a
  replayed event produces none; the syncing agent receives its own cards.
- **Cursor.** It moves only under compare-and-set and never backwards; a
  stale sync writes nothing.
- **Store and recipients.** Ingest is refused from an unbound or different
  store. A Mote actor that is not on the board is reported, not notified.
  Names that fold to `mote` cannot join.
- **Claims.**
  - The holders seeded on the first sync produce no cards.
  - A handoff reaches the new holder.
  - A displaced holder hears of a third-party handoff or a takeover after
    expiry, but not of their own handoff.
- **Bounded cards.** An oversized item is clipped and delivered; an invalid
  one is skipped, reported and retryable, and never stalls the cursor.
- **Timeouts.** Three in a row move the cursor to the latest event, through
  the CLI.
- **State keys and titles.** Reservation keys follow the contract
  (`rv:<rv>:<holder>:<deadline>:<phase>`), and a 25-path reservation still
  gets a short title.
- **Real `mote 0.1.0`.** Tested end to end, plus the review reproducers: a
  12-path reservation with long names expiring next to a handoff, a
  third-party handoff, and a takeover after expiry.
- **Warnings.** `brief` and `join` warn when `MOTE_ACTOR` names another actor.

Independent review of `ea93b08` raised three blocking objections, each
reproduced, and all are fixed here: one wide reservation stalled every later
sync, the displaced holder was not told, and the dedupe keys were event ids
rather than contract state keys. The review verified exactly-once delivery
under 6 concurrent syncs and 160 handoffs. Re-review then found that
replaying a chunk (an interrupted or concurrent sync) could send a false
"your claim is now …" card. Claim transitions now apply once, in op order,
and a replay test covers it.

Checks at this change: strict Clippy, 293 Rust tests, a locked build, the
three IPC scripts and `check_sql.py` all pass. `cargo fmt --check` reports only
`src/driver.rs`, which was inherited from `main` (fray #33).

## Mote adapter slice 5a: transport, binding, classification (2026-09-30)

`src/mote.rs` and `fray mote status` implement sections 1 to 4 of
`docs/design/mote-adapter.md`, approved at `b8c9677`.

`tests/mote.rs` has 20 tests, run with stub `mote` binaries and against the
real `mote 0.1.0` in scratch stores. They cover:

- classification from exit code, stderr and JSON together. The transport
  classifies real Mote rejections, `preflight` conflicts (exit 2 is a
  result), `events` (always an array) and clap usage errors (reported by
  their `error:` line).
- store location:
  - a board pairs implicitly only through an ancestor `.fray` or through the
    board the working directory's own repository selects;
  - a worktree outside the checkout still finds the main worktree's store;
  - bare repositories, including their linked worktrees, and submodules must
    name their store;
  - a repository nested in another is not taken for a submodule;
  - a bad `MOTE_STORE` names the variable and the path.
- refusal of a changed store id, both when binding and on every call;
- explicit `--store`, `--json` and `--actor` on every call, with no `--actor`
  for `events`;
- a timeout that kills and reaps Mote's whole process group, including when a
  leftover process keeps Mote's pipes open after Mote exits;
- `fray mote status`: a missing or unsupported Mote degrades to warnings, and
  without an identity nothing is written or read.

Independent review of the first version (`825d340`) raised five blocking
objections (fray #46–#50), each reproduced, and all are fixed here. 12
concurrent runs of the suite pass with no leaked processes.

Checks at this change: strict Clippy, 278 Rust tests, a locked build,
`integration.py`, `attention_integration.py`, `reliability_integration.py`
and `check_sql.py` all pass. `cargo fmt --check` reports only
`src/driver.rs`, which was inherited from `main` (fray #33).

## Finite waits without the hang-up floor (2026-09-30)

A finite wait now replies as soon as it has a result, instead of first joining
its hang-up observer, which can sit in a 100 ms bounded read. One atomic claim
settles what happens to input:

- Input that the observer claims before the waiter commits its reply is
  refused as pipelining (a protocol error).
- Input after the reply is committed begins the client's next request. It is
  handed back to the connection loop and served.

A client that follows the protocol, sending the next request only after
reading the reply, is never refused. The change is `wait_cancellable` in
`src/server.rs`, with regression tests in `tests/wait_latency.rs`.

The first version of this change (`0dcd56c`) lost a byte when input raced the
wait's wake-up. Independent review reproduced it in 51 to 75 of 300 to 400
stress iterations. The single-claim state fixes it: 0 bad on the reviewer's
seeds 1, 4 and 5. The regression test
`input_racing_the_wait_reply_is_refused_or_served_never_corrupted` repeats
the race 200 times, and it fails on `0dcd56c`.

Same machine and harness as the baseline below: release build of
`claude/epic-wave1`, 30 declared trials per row.

| Path | Priority | Before p50 / p95 ms | After p50 / p95 ms | Delivered after |
|---|---|---|---|---|
| wait (finite timeout) | p2 | 91.6 / 110.0 | 3.2 / 12.1 | 30/30, 0 duplicates |
| wait (finite timeout) | p1 | 98.4 / 110.9 | 2.9 / 13.1 | 30/30, 0 duplicates |
| watch --attention | p1 | 1.7 / 6.9 | 2.5 / 22.2 | 30/30, 0 duplicates |
| watch --attention | p2 | 104.2 / 107.3 | 107.5 / 121.0 | 30/30 (settle window, unchanged by design) |
| hook PostToolUse | p1 | 18.3 / 43.4 | 15.7 / 22.4 | 30/30 |

## Wake-path latency baseline (2026-09-30)

`scripts/bench_wake.py` (Mote bd-01M3RZ9BN4RYQRY56RFEPSTEJX, step 0) publishes
one question per trial through the real CLI, at a declared priority. It then times arrival at a
stand-in host on each delivery path, from the publisher's `send` returning to
the host-visible event, on one monotonic clock:

- **wait:** `fray wait --new` returns.
- **watch:** `fray watch --attention --notification` prints a notice. This is
  what the Claude Monitor plugin runs.
- **drive:** `fray drive` starts a stub child with the packet. The stub is
  Python and makes no model call.
- **hook:** `fray hook` PostToolUse returns context naming the item, assuming a
  tool boundary right after publication.

Model response and the host's scheduling of an idle session are not included;
they need real hosts, which the charter reserves for the owner. Failed or
timed-out trials count against a path. None occurred.

Release build `e8e6c93` (main `0ce4333` plus `fray stats`), macOS 14.3 arm64,
30 declared trials per path and priority:

| Path | Priority | Delivered | Duplicates | p50 ms | p95 ms | max ms |
|---|---|---|---|---|---|---|
| wait (finite timeout) | p2 | 30/30 | 0 | 91.6 | 110.0 | 121.9 |
| wait (finite timeout) | p1 | 30/30 | 0 | 98.4 | 110.9 | 122.9 |
| watch --attention | p2 | 30/30 | 0 | 104.2 | 107.3 | 108.5 |
| watch --attention | p1 | 30/30 | 0 | 1.7 | 6.9 | 22.0 |
| drive (stub child start) | p2 | 30/30 | 0 | 88.5 | 214.7 | 389.8 |
| drive (stub child start) | p1 | 30/30 | 0 | 72.8 | 171.2 | 292.5 |
| hook PostToolUse | p1 | 30/30 | 0 | 18.3 | 43.4 | 79.7 |

All 210 trials delivered. Duplicates can only occur on the watch path, which
emits a stream; there were none. A wait returns once, and each drive or hook
trial is a single invocation. Two floors appear, and they have different
causes:

- **watch at p2: deliberate.** The attention stream holds non-urgent items for
  a fixed 100 ms settle window (`settle_ms`, `src/server.rs` `watch_attention`)
  so that a burst arrives as one packet. p0–p1 items bypass the window and
  arrive in about 2 ms.
- **Finite waits: an artifact, independent of priority.** A finite wait's
  hang-up observer (`wait_cancellable`, `src/server.rs`) reads the client
  socket with a 100 ms timeout, and the reply waits for that thread to notice
  completion. `wait --timeout none` has no such observer timeout and gives
  p50 2.4 ms, p95 13.5 ms (20 trials). This is the first improvement target.

The drive column is dominated by process spawn. The stub records its time only
after the Python interpreter has started, so every drive sample includes
interpreter start-up, not just the tail.

Reproduce: `cargo build --release && python3 scripts/bench_wake.py
target/release/fray --n 30 --home-parent /tmp`.

## Collaboration stats baseline (2026-09-30)

`fray stats` and `fray friction` (Mote bd-01M3RZ9C6948DBVW30C1610F6W) replay the
event log read-only. This is the "before" measurement that the roadmap requires
ahead of the phases it judges. The baseline was taken from a read-only
`.backup` copy of the shared board at event 185 (2026-09-30), served by a
branch-built binary under a temporary home. The live board was not touched.

These figures replace a first draft taken at event 169. Independent review of
`c7ff681` objected to four metric definitions, all reproduced and now fixed:

- **O1.** Requests to the owner were reported as unreachable.
- **O2.** A bystander's note counted as the answer.
- **O3.** Unacked age ran from a card's newest event, not from the oldest
  unacknowledged one.
- **O4.** A re-showing could stand in for a pruned first showing.

The live board is at schema version 3, written by the installed binary built
from the unmerged `codex/pairing-friction` branch (d3b1480). A `main` binary
refuses to open it (`schema_version`). Version 3 only adds tables
(`peer_generations`, `peer_seen`, `review_subjects`, `review_verdicts`), so the
private copy was relabelled version 2 for this read-only measurement.

| Metric (all history, 185 events) | Value |
|---|---|
| Asks created / responded / resolved / open | 10 / 7 / 3 / 7 |
| Ask first response p50 / p90 / max | 8m / 38m / 38m (n=7) |
| Ask resolution p50 / max | 14m / 17m (n=3) |
| Oldest open ask; oldest unanswered | 7.0d; 6.9d |
| Objections raised / resolved / open / overridden | 12 / 8 / 4 / 0 |
| Objection resolution p50 / max | 12m / 17m (n=8) |
| Reassignments (possible misroutes) | 4, on 4 cards |
| Publish to first shown, p50 / p90 / max | 13s / 2m / 31m (n=28, certain first showings only) |
| Unacked attention now | 46 items across 4 agents; 3 absent agents hold 39, the oldest 7.0d |
| Lanes taken / live | 0 / 0 |
| Friction notes | 0 |

The four open objections are the review of this very change. `fray friction`
also lists two asks (#19 and #21) to `codex-attention-0923` that have been
unanswered for 6.9 days, and reports that nothing can reach that agent (five
open requests). The most visible weakness is stale obligations to absent agents,
not slow answers between agents who are present.

The store does not measure, and the command says so: host wake and model
response latency (epic child 3), acknowledgment latency over time, truncation
refetches, reviews per landing (child 1) and lane handovers.

Checks at this change: formatting, strict Clippy, 227 Rust tests (17 in
`tests/stats.rs`, 3 in `tests/wait_latency.rs`), locked build,
`integration.py`, `attention_integration.py`, `reliability_integration.py` and
`check_sql.py` pass.

## Phase 1 client and attention candidate (2026-09-23)

At source manifest
`cfb75235bc6fd90e3885e194cd8f6b744f82500a6f5d0bb801abef02eaa94b64`,
formatting, locked check, strict Clippy, **135 Rust tests** and locked build pass.
A frozen binary (`4ef98a5df9147dd2a58079de54b87f33f2a5a14e1d40e2656ced88d16d80d936`)
passes **76 IPC tests**: 13 new Phase 1, 32 legacy, 24 attention and 7 reliability.
Python resource warnings are errors. SQL validation prepares 75 static statements
and passes 22 checks; bundled skill and Claude plugin validation pass.

Evidence is `/tmp/fray-phase1-candidate-v4/` (frozen binary, 46-file source manifest,
IPC logs and exit metadata), `/tmp/fray-phase1-cargo-v4.log`, and
`/tmp/fray-phase1-sql-v4-fixed.log`. The manifest includes source, tests, scripts,
integrations and bundled skills. Validation documentation itself is excluded.

The new cases establish host-neutral precedence and stable identity across fresh
CLI processes, inherited enter/drive binding even when child provider IDs differ,
old-daemon field stripping, quiet default waits, card-filter equivalence and
unrelated-traffic isolation, mute/unmute with exact unread receipts, direct linked
objections remaining visible, and no routing after leave. Hook payload tests cover
PreToolUse/PostToolUse ordering, FRAY_SELECTION, no implicit rejoin, and one-shot
Stop warnings. Store tests distinguish manual/boundary, expired, filtered and
armed activation, including controller expiry and the brief byte budget. A post-gate edge probe
found the first candidate returned 2,042 bytes for a 2,000-byte brief with an
80-character actor name. The corrected candidate adds a regression and drops
optional readiness detail after exhausting rows, retaining the warning and arm
command. All Cargo and IPC gates above were rerun on that correction.

Cross-review on Fray #13 found two further blockers: a direct question created
and closed while the recipient was away did not return on rejoin (#17), and the
assigned objection card itself could be muted (#18). Rejoin now covers terminal
direct routes; active assigned questions cannot be muted, and previous mutes are
ignored if a card later becomes an assigned question. Bare unmute no longer
creates an unrelated receipt, and catch-up includes missed peer events preceding
a later own reply. Store and CLI regressions cover these cases.

An additional IPC regression failed against the v2 frozen binary because unmute
did not notify an already blocked waiter when no event sequence changed. The
server now signals mute/unmute selection changes under the existing lock. The
regression arms the waiter first and requires the restored receipt within one
second. Its failing evidence is `/tmp/fray-phase1-unmute-baseline.log`; it passes
in the final Phase 1 IPC log. All Cargo/IPC results above cover these review fixes.

These are local macOS tests with synthetic host payloads and ordinary Python
children. New native host hook installation, automatic idle wake across every
host, and Linux execution are not claimed. The shared daemon and installed
binaries were not changed by this lane.


## Cooperative improvements: final candidate (2026-09-23)

Final source manifest:
`36191df61576d7f21dbcc9c4c8af6b41b454584492684c07721e790ed77863c9`.
Frozen binary:
`053340d179d8c3d2a974a22aaed7f026364d5c9a18b3edcabbb932f1320c26bc`.

Cargo format/check/Clippy/build and **113 Rust tests passed**. SQL validation
prepared 64 static statements and passed 22 checks; plugin validation passed.
All **63 IPC tests passed**: 32 existing integration, 24 attention/adapter, and 7
reliability cases, with resource warnings treated as errors. Results are recorded
in `accepted-*-ipc.log` under `/tmp/fray-cooperation-round2/`; the manifest covers
code, tests, adapters and skills and remained unchanged throughout those runs.

Peer review reproduced three additional defects: descriptor exhaustion could
terminate the daemon at a 256-descriptor limit, doctor omitted two adapter
capabilities, and activation metadata prevented old-daemon notification fallback.
The final implementation derives admission from the inherited descriptor limit,
retries accept failures with backoff, sends busy on socket-clone allocation
failure, reports missing activation/batch capabilities and degrades optional
activation metadata with an explicit warning. At limit 256, admission becomes 74
clients, including at most 58 long-lived consumers and 16 reserved short slots.
A regression saturates that configuration with finite waits and verifies busy
exit 4, daemon survival, send/inbox/ACK availability and total-client cleanup.
The descriptor probe is a fixed, environment-cleared `/bin/sh -c 'ulimit -n'`
call at startup. No limit is raised; dependency count remains five and
`forbid(unsafe_code)` remains intact.

Claude independently verified the fixes on Fray #8 at sequence 92 and resolved
objections #9–11. Its separate saturation run admitted 58 consumers and returned
explicit busy responses to 72 more at limit 256, while 20 parallel pings and
send/inbox/ACK succeeded. It also checked doctor and both adapter modes against
the real older baseline daemon. The reviewed scratch copy matched the final
client, diagnostic, notification and dependency files; a small subsequent
server delta turns socket-clone failures into busy responses and is covered by
the final gates above. Any peer addendum on that delta is recorded in thread #8.

The final compact-thread comparison is **44,870 to 19,985 bytes (55.46% smaller)**
for the same 23-event review conversation. The extra omission marker appears in
both views; all bodies, references, receipts and checked context still match.
Follow-up lists over 50 now expose a continuation, and passive batch registration
does not refresh agent activity. Batch registration can precede failed stdout;
it proves neither host exposure nor handling. ACK always remains explicit.

The native Monitor receipt below is bound to the earlier frozen binary, whose
notice format is unchanged by these review fixes. Automatic plugin loading,
other interactive hosts and Linux execution remain unqualified locally. The
shared daemon and global binary/plugin installation were left unchanged.

## Cooperative reliability and reading checkpoint (2026-09-23)

At source manifest `de60cf4004b63a7f10e0a152d7426b9a57caac3a8247d0ef6a1352a12064cfd9`,
Cargo format/check/Clippy/build and 111 Rust tests passed. All IPC checks used a
frozen binary (`5e0effe697421749c69c2674b3b91047af0eba8adb5f894084f83d5c37159103`)
on temporary homes: 23 attention/adapter, 32 existing integration, and 6 connection
reliability tests passed without resource warnings. SQL validation prepared 64
static statements and passed 22 checks. Plugin manifest validation passed.

The reliability tests cover 112 simultaneous long-lived waiters while short
send/inbox/ACK operations remain available, transient admission exit 4, 140
cancelled finite waiters, finite connection reuse, half-close cancellation and
unexpected inbound bytes. Independent read-batch tests cover interleaved readers,
restart persistence, wrong-store tokens with matching actor/card numbers,
continuation before acknowledgment and compact summary/reference preservation.
The pagination and summary tests failed before their respective peer fixes.

A SQLite backup of our actual review thread (#2) was opened only on an isolated
home. The ordinary JSON was 44,846 bytes; compact JSON was 19,961 bytes, a **55.49%**
reduction using identical serialization. All 23 full message bodies, references,
sequence/actor/operation/timestamp fields, current head, receipts and continuation
metadata matched. This is a measured result for that conversation, not a promised
ratio for every thread. The snapshot also exercised additive schema initialization
against the old board database without upgrading the shared daemon.

`doctor` was tested against both fixture and old shared daemons. The first
old-daemon trial caught an unsupported roster argument; the corrected diagnostic
uses the old bounded roster call. Database snapshots verify that diagnostic reads
create neither acknowledgment nor presentation records. The existing Claude
session then passed the native Monitor check for the compact notice: it reported
an idle wake on a complete notice, fetched the immutable batch and full thread,
and acknowledged exactly card 1 through sequence 1. We independently verified
that ACK and the stopped listener, then shut down the owned fixture daemon.
Evidence is `native-notice-acceptance.json`, with the peer report on thread #1 at
sequence 81. This qualifies the notice on that host; automatic plugin loading
and other interactive hosts remain outside this evidence.

Raw logs, source hashes, frozen binary and comparison artifacts are retained in
`/tmp/fray-cooperation-round2/`. This checkpoint precedes the final follow-up-list
omission review. The shared daemon and global binary/plugin installation remain
unchanged; these are local macOS results, not a hosted Linux CI run.

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
source copy had no Git metadata at that checkpoint; these results describe local source, not a
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
