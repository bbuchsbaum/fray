# Independent Astra correction review — 2026-10-07

## Exact verdicts and scope

- **Fray APPROVE:** `19084e8291864e12387b47be6632a455a720888d`, correction baseline `3b1ae994d1f7b4578c8d0f6979e6d1b4f2f7ffce`, checkout `/private/tmp/fray-outstanding-motes`.
- **Mote APPROVE:** `b114f4b731a9f6836116054df7084a7dff0e90e7`, correction baseline `a86803de31e890ac6e4331c9961705cabcde2fcb`, checkout `/private/tmp/mote-fray-prerequisites`.
- **Combined advisory APPROVE** for this exact pair on Fray #109, subject revision 2. All three original actionable findings are addressed. No new actionable correctness defect was found in the correction review; this is not exhaustive proof or landing authority.

The original full-session baselines remain Fray `b2cd1b9a187a5f9be14f66924e164732c3e3d979` and Mote `63454fe71036c8e0eecd7ee2db6e84f0b7ba29a1`. This review extends the earlier independent full-session review by inspecting the successor changes, complete affected control paths, callers, contracts, and regression coverage. It does not repeat every unchanged subsystem review. Both review checkouts were clean and matched the stated heads when inspected.

## Findings disposition, in original severity order

### P1 Mote: same-writer publication could overwrite recovery evidence — fixed

`src/authority.rs:338` now recovers an outstanding publication under the retained writer lock before creating a replacement journal. Recovery failure propagates before a new journal or operation is written. `src/cli.rs:9399`, `:9424`, `:9427`, and `:9444` propagate compensation/note publication errors, stopping compound `begin` rather than publishing later work after a failure. Previously admitted work remains admitted.

The two library tests at `src/authority.rs:416` and `:440` construct a linked pending publication through the production writer/publisher. They check same-writer admission order, no stranded journal, successful later writer acquisition, and exact original journal preservation when recovery refuses inconsistent bytes; the new operation must remain absent. These assert the durable failure boundary rather than merely the presence of a recovery call. The correction is prospective: it does not claim to repair stores already stranded by the old bug. Detailed Mote-only review is retained in `astra-mote-review.md` alongside this report.

### P2 Fray: accepting a withdrawn packet still changed Mote ownership — fixed

`src/dispatch.rs:32` validates the intended recipient and all terminal card states for `for_accept`. `src/dispatch_client.rs:454`, `:547`, and `:608` require this daemon validation before preparing new adoption and immediately before each new carrier-close/adopt mutation, including resumed operations. An older daemon that omits explicit confirmation fails closed (`:464`).

Readback-only recovery of already adopted reservations remains possible. Completed exact-key retries return historical receipts (`:502`); once all ownership steps are observed committed, finalization can record them historically without reopening the withdrawn card (`:643`). New-key acceptance of a terminal packet refuses. The operation context and reservation/path/holder checks remain in place. Cancellation is checked between stores, not made atomically authoritative over an already-started Mote command; the contract states that limit.

The new daemon regression checks wrong recipient and withdrawn/resolved/superseded states. The real CLI script checks withdrawal before acceptance, after carrier close, and after adoption; it checks reservation actor/entity, carrier status, unchanged packet status, completed retry lease stability, and refusal of a new key. Its after-close retry also compares carrier history to establish no repeated mutation.

### P2 Fray: same-sync feed/reconciliation duplicate handoff notices — fixed

`src/main.rs:1693` performs a fresh claim/issue read after the board observation, starting from the sync's original admission cursor. `src/mote.rs:552` requires the exact consumed final transition to remain the latest relevant raw operation for that entity in admission order. Later holder cycles, release, issue status changes, unknown kinds, or malformed evidence disqualify coverage. Earlier-spelled operation IDs are not treated as older admissions. Unrelated entity changes do not unnecessarily invalidate coverage.

`src/store.rs:2439` additionally checks current cursor, exact feed operation/holder, and an existing delivered recipient card under the existing sync-generation and snapshot-holder CAS. It updates the snapshot normally, suppressing only redundant attention. Feed and snapshot projections remain separate, so late feed catch-up cannot overwrite a newer snapshot observation. Failed verification, missing recipient delivery, stale proof, or older daemon capability retains ordinary reconciliation. No op-ID comparison against synthetic snapshot markers is used.

The regression schedules exercise unseen ABA and close/reopen cycles, earlier-spelled admission IDs, unrelated entities, stale operation/cursor proofs, absent recipient delivery, independent snapshot-ahead state, and quiet retry. The public fixture additionally exercises verification-read failure. The real-Mote court asserts exactly one recipient handoff notice and no sender reconciliation notice.

## Independent checks and retained evidence

I independently built the frozen Fray source into `/private/tmp/fray-astra-fixes-20261007/astra-target` and ran:

```text
cargo test --locked --offline --test dispatch --test mote_admission -- --skip public_admission_feed_compatibility_court
```

Result: **17 passed, 0 failed**, with the subprocess compatibility test deliberately filtered. Complete output and actual exit status 0 are in `astra-targeted-default-cache.log` and `astra-targeted-default-cache.meta.json`. The first attempt selected an incomplete alternate Cargo cache and failed with exit 101 before compilation (`fs2` unavailable offline); that output/status is preserved in `astra-targeted.log` and `astra-targeted.meta.json`. The successful retry used the available default cache. No dependency install or fault injection was performed.

I verified all **20** manifest entries, **111** current source-input hashes, and **7** decompressed raw-log hashes, and compared the retained Mote patch byte-for-byte with the exact correction diff. Results are in `astra-evidence-check.json`. Inspection of complete retained summaries/status sidecars confirms Fray 456 Rust passes, 96 Python passes, the affected 9-test admission rerun, required checks, and Mote 425 passes with 2 ignored. These are implementation-run results, not independent reruns. Full Fray Rust qualification preceded two test-only lint corrections; the affected tests were rerun afterward.

I also inspected both clean postcommit build logs/status sidecars (exit 0), the postcommit real dispatch court (10 cases, exit 0), and the candidate binary version stamp `fray 0.2.1 (19084e829186)`. The CLI court retains aggregate successful assertion output, not a complete transcript of each successful subprocess/readback; I inspected its assertions in source. No new injection commands were run by this reviewer.

## Limits

No new native host, paid model, hosted CI, soak, deployment, installation, shared-daemon restart, or original-store migration is qualified. No implementation or Mote ticket changes were made by this reviewer. The original baseline journal regression classification remains source-supported rather than a baseline runtime reproduction. Same-sync duplicate suppression intentionally does not persist a cross-restart association: a crash after feed ingestion but before reconciliation can produce a conservative later notice. Board/feed observations are not ownership fences. Concurrent cancellation cannot undo an already-started ownership mutation. Unchanged original-sweep areas retain the coverage limits in the original review report.
