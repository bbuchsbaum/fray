# Independent exact-candidate review

Review date: 2026-10-06 America/Toronto (evidence timestamps extend into 2026-10-07 UTC). Reviewer: `astra-fresh-review-20261006`. Coordination: shared Fray review #109, subject revision 1. This is a read-only review; no product changes or acceptance transitions were made.

| Repository | Baseline | Candidate | Verdict |
| --- | --- | --- | --- |
| Fray | `b2cd1b9a187a5f9be14f66924e164732c3e3d979` | `3b1ae994d1f7b4578c8d0f6979e6d1b4f2f7ffce` | **OBJECT** |
| Mote | `63454fe71036c8e0eecd7ee2db6e84f0b7ba29a1` | `a86803de31e890ac6e4331c9961705cabcde2fcb` | **OBJECT** |

Combined verdict: **OBJECT**. The findings below concern the frozen candidates, not later revisions. P1 means a high-priority correctness defect; P2 means a normal-priority actionable defect.

## Findings

### 1. P1 — Mote: reusing the begin writer after an ignored publication failure can strand an operation and make the store unreadable

**Candidate location:** `src/cli.rs:9443`, followed by `src/cli.rs:9460`. Relevant publication boundary: `src/authority.rs:325-337`; recovery on writer acquisition: `src/authority.rs:219`.

**Trigger:** On an authority-enabled store, run `mote begin work --paths src/a --note TEXT --announce review-topic`. The optional note's operation file reaches `ops/`, but publishing its admission record encounters a transient I/O error. This is an ordinary publication failure that the durable `publication.json` is intended to recover.

**Observed:** The note publication error is discarded. The subsequent announcement uses the same `Writer`, whose `publish` method validates the pending note but does not recover it before replacing `publication.json` with the announcement. The announcement is admitted and its journal removed; the note remains outside the authority ledger. The begin command, a subsequent `show work`, and a subsequent `note` writer all exit 1 with `unadmitted operation ...; fenced stores require the shared Mote publisher`. Normal next-writer recovery cannot repair the store because its missing journal has already been overwritten.

The independent fixture injected exactly one EIO at the note admission-record rename, using the unmodified freshly built candidate executable and a process-local test interposer. Ledger inspection confirms that create, reserve, claim, status patch, and announcement are admitted; the intervening note is not; no publication journal remains. The original stores were never used. No further injection was performed after the review was instructed to stop fault-injection work.

**Expected:** A transient optional-note failure may be reported, or the exact pending operation may be recovered before continuing, but it must never destroy the only durable recovery record or strand the store.

**Smallest plausible correction:** Propagate the note publication error and stop the compound command, preserving its journal for the next writer. More generally, prohibit any same-writer publication after an unresolved earlier failure, or recover the existing publication under the held lock before publishing another operation. Audit the other ignored compensation publications in `cmd_begin` for the same pattern.

**Baseline classification:** Candidate-introduced by source comparison. Baseline `cmd_begin` used `publish::publish_op` separately for the note and announcement; each call went through `publish_bytes`, acquired a fresh Writer, and recovered pending publication before the next operation. The candidate changes those calls to a single retained Writer while keeping the ignored note error. **Baseline behavior was not experimentally replayed**; the regression attribution is a source-supported inference, while the candidate failure is directly reproduced.

**Evidence:** `mote-transient.raw.json`, `mote-transient-results.json`, `mote-transient-ledger.json`, `mote-transient.log`, `mote-transient.status.json` (reproducer exit 0). `mote-transient.py` records the exact public commands; the existing `fail-rename.c` records the one-shot injected boundary. These artifacts are retained for inspection, not a request to repeat injection.

### 2. P2 — Fray: accepting a withdrawn handoff packet still closes its carrier and adopts its reservation

**Candidate location:** `src/dispatch_client.rs:442-460` and `src/dispatch_client.rs:482-529`. The handoff branch bypasses ordinary dispatch acceptance at `src/dispatch_client.rs:163-164`.

**Trigger:** Alice holds a work claim and a live reservation on a separate carrier issue, then sends a structured handoff to Bob. After transfer, Alice withdraws the Fray packet before Bob accepts it. Bob subsequently runs `fray --key adopt-withdrawn accept PACKET_ID` using the stale packet ID.

**Observed:** Acceptance exits 0 and returns `state: completed` with `confirmed: true`. It closes the carrier in Mote and moves the reservation to Bob's work issue, even though the packet remains `status: withdrawn`. The fixture's packet was #2 and reservation `rv-01M4A3MM30RE1P5SQ93DCS2VWF`. This was a withdrawal completed before acceptance began, so the failure does not depend on a narrow concurrent race.

The packet retrieval contains payload/status for the handoff workflow but does not enforce the terminal status of its Fray card. `accept_packet` checks the recipient and prepares a journal; `execute_adopt_steps` checks live Mote ownership, then begins closing carriers without validating cancellation. Ordinary offers have explicit terminal-card checks in `dispatch::accept` and `dispatch::confirmed`.

**Expected:** A new acceptance of a withdrawn/resolved/superseded handoff should refuse before new Mote mutations. Recovery of an already-started adoption needs an explicit cancellation/recovery policy; it should not silently treat cancellation as renewed permission.

**Smallest plausible correction:** Add a daemon-side packet acceptance/terminal-state validation before preparing a new adoption, and revalidate pending adoption before additional external mutations. Preserve already-committed receipts as historical results rather than rolling back Mote ownership automatically.

**Baseline classification:** New workflow introduced in this candidate range; `dispatch_client.rs` and structured handoff acceptance did not exist at the Fray baseline.

**Evidence:** `public-cli.raw.jsonl` contains every command, complete stdout/stderr, and individual exit code. `public-cli-results.json` contains acceptance plus post-action packet, carrier, and reservation readbacks. `reproduce.py` contains the sequence. All commands used the independently rebuilt candidate binaries.

### 3. P2 — Fray: each ordinary admitted handoff can generate duplicate recipient attention and an unnecessary sender notice

**Candidate location:** `src/store.rs:2365-2372`, with reconciliation at `src/store.rs:2441-2467` and its caller in `src/main.rs:1688-1748`.

**Trigger:** Seed Fray synchronization while Alice holds work, perform a normal holder-checked handoff to Bob, then run `fray mote sync` once on the authority-enabled store.

**Observed:** The same handoff produces both `Mote: alice handed you work` and `Mote: you now hold work`, addressed to Bob. It also tells Alice `Mote: work is now held by bob`, describing the change as discovered by reconciliation even though the feed just supplied the exact handoff and Alice performed it. These were separate durable cards #3, #5, and #6 in the fixture, from one sync. An unrelated new carrier claim accounts for card #4 and is not counted as duplicate evidence.

The new admission path updates `mote_feed_claims`, but updates reconciliation's `mote_claims` only for seed events. Reconciliation therefore compares the live Mote board to a stale holder and reports the same transition with a different key. Per-key retry deduplication cannot collapse these notices. The retained admission CLI court tests quiet retries but does not assert that the first sync produces only the intended handoff notices.

**Expected:** An already-observed handoff should generate its intended attention once. Reconciliation should add attention for changes not already represented by the feed, while preserving the ability to detect later independent changes.

**Smallest plausible correction:** Coordinate the two projections using an admission-aware observed transition or deduplication token; keep reconciliation current when the feed's state is the state just verified. Avoid restoring lexical op-ID ordering or blindly overwriting a newer board observation.

**Baseline classification:** Candidate-introduced by source comparison: baseline feed and reconciliation updated the same `mote_claims` table. The candidate splits the projections and only synchronizes them for seed events. No baseline runtime comparison was run.

**Evidence:** `public-cli.raw.jsonl` and the `handoff_sync` section of `public-cli-results.json` preserve all created card IDs, titles, recipients, and the sync response.

## Independent checks and provenance

Both repositories were built directly from the clean review worktrees using `cargo build --locked --offline`, with separate target directories wholly inside this review directory. Both builds exited 0; complete logs and actual statuses are in `fray-build.log`, `fray-build.status.json`, `mote-build.log`, and `mote-build.status.json`.

The preexisting Fray debug executable reported an older build stamp and was not used for the reproductions. Independently built Fray reports `fray 0.2.1 (3b1ae994d1f7)`; its SHA256 is `209203de02b300ed9ade66a80b40660396b8d3633555f1d7d068229046b8af96`. Independently built Mote SHA256 is `53a9c57e53f6a617de84c30c7d0bb4baec83e1f7aab5ac3bbdbfae417e9074ad`; its package version alone does not identify the commit. `independent-provenance.json` binds both build paths to the checked source heads and records empty tracked status after the work.

The public CLI fixture completed and retained all per-command exits and state readbacks. Its outer shell status-recording wrapper then failed because `status` is a read-only zsh variable; this is a harness bookkeeping failure, not a product failure. No aggregate green test status is claimed for that wrapper. The product exits cited in findings 2 and 3 are directly captured by Python's `CompletedProcess.returncode`. The owned daemon stopped normally (exit 0), and its socket was removed, as recorded in `public-cli-cleanup.json`. No test daemon remains.

## Coverage and limits

I reviewed changed product implementations and relevant callers/contracts for Mote claim/begin admission, authority status and journal publication, fenced landing current/historical receipts and recovery, Fray operation journals and session-independent receipt lookup, candidate review/successor/landing, dispatch acceptance and carrier adoption, admission synchronization, retention/archive/restore, review/objection routing, keepalive turn claims/re-fork/token accounting, metrics, diagnostics, snapshot hashing and schema additions. I inspected relevant tests and the public CLI courts to identify their acceptance boundaries. Depth was greatest in the authority and workflow paths, where the independent reproductions were run.

This was not a line-by-line proof of every changed test or documentation file. The full main CLI hook/rendering integration, all existing escalation/controller permutations, and all filesystem/concurrency interleavings were not exhaustively exercised. No broad test-suite rerun, new long soak, native Claude/Codex trial, browser/visual qualification, physical power-loss test, hosted CI inspection, or paid trial was performed. There is no release, installation, merge, push, or live-store qualification claim.

Existing soak evidence is explicitly tied to Fray `9be4977`, not this final binary. Existing workflow evidence names corrected source `883b93a` and records local/synthetic scope; earlier green counts do not cover the discriminating failures above. I formed the findings before consulting retained evidence summaries and did not use their earlier approvals as review authority. I found no additional actionable defect in the source paths examined, but that is not exhaustive assurance.

All implementation checkouts and original Mote stores were preserved. Scratch source, logs, fixture stores, build output, and this report are confined to `/private/tmp/fray-astra-fresh-review-20261006`. The failed Mote fixture is retained as evidence rather than repaired or activated anywhere else. Mote tickets remain unchanged.

## Review delivery

Combined exact-SHA OBJECT verdict published on shared Fray #109 at event @553, subject revision 1; the protocol created linked objection #110. Exact delivered request batch was acknowledged through @552. Reviewer identity `astra-fresh-review-20261006` then left the board. No other identity was changed.
