# Pairing friction assessment — 2026-09-26

The fmrialigned pairing found real value in independently reproduced objections,
scope negotiation, and guarded closure. Preserve those mechanisms. The strongest
Fray improvements reduce the work between seeing evidence and responding to it.

Sources read in full:

- `/Users/bbuchsbaum/code/fmrialigned/.git/fray-pair/claude-fray-notes.md`
- `/Users/bbuchsbaum/code/fmrialigned/docs/validation/fray-pairing-2026-09-26.md`

The logs report different durations (Claude's roughly 45 minutes versus the
receipt timeline's roughly 18 minutes from hello to integrated result). Neither
is a controlled productivity comparison. The reported third of calls spent on
bookkeeping is useful field feedback, not an independently measured rate here.

## Implemented slice

Base: `1673d61`, isolated branch `codex/pairing-friction`. No live daemon or
fmrialigned state is changed by these edits.

| Finding | Source diagnosis | Change |
| --- | --- | --- |
| Notifications repeat the opening title | `notification::notices` used only the card title despite messages already present in the attention packet | Add a bounded latest peer message preview, constrained by the exact receipt; retain title and receipt identifiers |
| Read/reply/ack costs separate calls | Reply and acknowledgement were separate CLI operations | `reply --ack-batch TOKEN` atomically replies and acknowledges only that card's recorded version; invalid receipt or failed reply rolls back both |
| Two batches appear to require two acks | Acks already advance per-agent/card sequence; batches are immutable references, not separate queues | Clarify overlap semantics and regress watch/thread overlap; no automatic acknowledgement |
| Replies cross newer peer updates | Annotation writes did not inspect the reader's explicit receipt history | Warn when updates exceed acknowledged history or this session's inbox/thread receipts; background watches/waits cannot hide the warning |
| Pending typo remains in roster | `send --pending` inserts a disabled recipient permanently; roster listed all rows | Hide never-joined recipients after all addressed work is terminal or rerouted; `agents --all` preserves historical visibility |
| Revision lookup requires parsing | Human thread headers omitted revision | Print `rREV`; retain numeric compare-and-swap and objection gates |
| Roles and idle timeout unclear in help | Role values were validated only by server; drive timeout had no explanation | List valid roles in join/enter help and explain idle versus running-child timeout |
| Triple ownership bookkeeping | Fray instructions prescribed a lane before separately describing Mote authority | Use `mote begin` for Mote ownership; avoid requiring duplicate Fray claims or writer lanes for read-only review |

The reply warning is advisory and conservative. It does not reject a reply or
establish comprehension. Plain history reads do not create explicit pull
receipts; unbound sessions and old/expired receipts may also cause warnings.
Conversely, an inbox
receipt can cover truncated context. These are reasons to describe recorded
exposure precisely, not to call the warning an unread-message guarantee.

## Remaining work implemented

The continuation implements all three follow-up contracts:

| Contract | Behavior and evidence |
| --- | --- |
| Peer discovery | Session-local peer generations surface exact names, roles, recent activity and listener state on entry/tool boundaries. Successful output marks only the shown generations; failed or budget-deferred output leaves notices unseen. Leave/rejoin and session replacement produce fresh notices. Stop does not block for roster news alone. Store tests cover generations, takeover, phantom exclusion, limits and session isolation; CLI tests cover hook delivery, repeat suppression, output failure, budgets and Enter-to-child delivery. |
| Working-tree snapshots | Explicit literal paths capture working bytes, modes, tracked deletions and nonignored untracked files. SHA-256 identifies a deterministic manifest and copied evidence under `BOARD_HOME/evidence/snapshots`. Verify checks identity, inventory, bytes and modes. Tests include the reported 22-untracked-file case, binary data, staged deletion, symlinks, Gitlinks, conflicts, tampering, output overlap and deterministic concurrent source/deletion changes. |
| Versioned reviews | A request freezes scope and baseline, tracks an author-controlled current candidate with subject revision CAS, and records exact-version advisory verdicts. Superseded verdicts stay visible and stale through A-to-B-to-A. Stale verdicts cannot post or acknowledge. Existing linked-objection closure rules remain intact. Tests cover CAS, authority, atomic failure, Unicode bodies, persistence and bounded attention payloads. |

Commands and limits are documented in [EVIDENCE.md](../EVIDENCE.md). Raw test logs
still belong in a shared agreed directory and should be referenced explicitly.
Fray creates source bundles but does not transfer them or collect arbitrary logs.
A manifest covers the selected scope, not an inferred whole repository. Capture
revalidates ordinary concurrent edits but is not filesystem-atomic. Review
references are declarations whose artifacts must be verified separately.

Schema version 3 adds the four presence/review tables while preserving existing
board identity, cards and receipts. Older daemons refuse the upgraded database;
otherwise they could bypass frozen review scope. CLI feature negotiation keeps
explicit use from silently degrading against older daemons. No live board was
upgraded by this work.

Independent review reproduced and drove fixes for bundle symlink reuse,
recreated deletions, missing paths beneath symlink ancestors, unpopulated
Gitlinks, pre-read manifest validation, tab-containing conflicted paths, brief
byte-budget overflow, Enter consuming child notices, Unicode request summaries,
and old-daemon scope bypass. Tests also caught first-use output directory
creation. Snapshot tests independently exercise changed existing files and
recreated tombstones, asserting failed capture leaves no published bundle.

## Other feedback

- A Fray/Mote bridge that writes both stores needs partial-failure semantics;
  `lane take --for mote:ID` alone would not make the two writes atomic. Simplify
  the workflow first; a future bridge should delegate authority to Mote and
  display its result without independently claiming success in Fray.
- Fray `resolved` records a conversation outcome. Mote completion records work
  acceptance. Do not alias them into one automatic close operation. The bundled
  fixed/unfixed issue was a scope/acceptance mistake caught by peer review.
- The existing project-local skill installer has no explicit user-scope flag.
  A separate `--scope user` is reasonable future work with the same conflict
  checks; this work does not alter global installations.
- Omitted raw logs and shell environment-variable placement are workflow
  mistakes, not storage failures. The evidence-bundle convention helps with the
  former; no Fray parser workaround is warranted for the latter.

## Initial slice verification

The initial slice passed the checks below on macOS. These logs describe the first
turn; the continuation has a separate verification record below. Tests used
isolated temporary boards only.

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Pass |
| `cargo check --locked --all-targets` | Pass |
| `cargo clippy --locked --all-targets -- -D warnings` | Pass |
| `cargo test --locked --all-targets` | 213 passed, 0 failed, 0 ignored |
| `cargo build --locked` | Pass |
| `python3 scripts/integration.py BINARY` | 34 passed |
| `python3 scripts/attention_integration.py BINARY` | 27 passed |
| `python3 scripts/reliability_integration.py BINARY` | 7 passed |
| `python -m unittest discover -s integrations/codex-wake -v` | 23 passed |
| `python3 scripts/check_sql.py` | 101 statements prepared, 22 checks passed |
| Skill validation of `skills/fray` and `skills/fray-review` | Both valid |
| `git diff --check` | Pass |

Builds used `CARGO_TARGET_DIR=/private/tmp/fray-pairing-target`; `BINARY` above
is `/private/tmp/fray-pairing-target/debug/fray`. The wake suite used an isolated
Python environment with the repository's pinned websockets dependency and
`FRAY_WAKE_TEST_BINARY` set to that binary. Its first discovery attempt used the
default binary path and failed before loading the integration module; rerunning
with the supported override passed. No application fix was needed for that
environment mismatch.

Full logs and exit-status sidecars are in
`/private/tmp/fray-pairing-friction-20260926/`: `final-fmt.log`,
`final-check.log`, `final-clippy.log`, `final-rust-tests.log`, `final-build.log`,
`ipc.log`, `attention.log`, `reliability.log`, `codex-wake-final.log`, and
`sql.log`. The directory also contains `changed-files.sha256.json` and
`review.patch`, including both new files, for reviewing this uncommitted slice.

Independent source review found one session-isolation issue in the warning:
NULL sessions shared read markers. It was fixed, covered by a regression, and
rechecked by the reviewer. Added acceptance tests also cover a post-ack failure
rolling back the transaction and combined `reply_refs`/`reply_ack_batch`
capability negotiation. No findings remained in that bounded follow-up review.

Local checks do not establish Linux CI or real-host wake behavior. The Codex
host in adapter tests is a mock. This branch is uncommitted, not installed, and
not merged into main; no main-landing approval is implied by the source review.

## Continuation verification

All continuation gates passed on macOS, against the complete initial plus
remaining slice. There are 328 passing automated tests in the suites below,
with no failures or ignored/skipped tests.

| Check | Result |
| --- | --- |
| Formatting, all-target check, Clippy with warnings denied, locked build | Pass |
| `cargo test --locked --all-targets` | 232 passed |
| General CLI/IPC integration | 38 passed |
| Attention integration | 28 passed |
| Reliability integration | 7 passed |
| Codex wake adapter (mock host, real local Fray) | 23 passed |
| Static SQL validation | 115 statements prepared; 22 checks passed |
| `fray` / `fray-review` skill validation | Both valid |
| `git diff --check` | Pass |

Logs with exit-status sidecars are in
`/private/tmp/fray-pairing-remaining-20260926/`: `final-fmt.log`,
`final-check.log`, `final-clippy.log`, `final-rust-tests.log`, `final-build.log`,
`ipc.log`, `attention.log`, `reliability.log`, `codex-wake.log`, `sql.log`,
`skill-fray.log`, and `skill-fray-review.log`. The same directory holds
`changed-files.sha256.json`, `review.patch`, `snapshot-create.log`, and
`snapshot-verify.log`. The snapshot logs identify the complete changed-file
bundle, including all untracked modules, tests and documentation; the patch
also includes untracked files. These artifacts supersede the first-turn hashes
for the current candidate.

The early `ipc-focused.log` records a real first-use output-directory failure;
the snapshot module was fixed and its regression plus the full IPC suite now
pass. Rust tests emitted Perl locale fallback warnings from `shasum` under this
shell's unsupported `C.UTF-8`; all tests completed successfully. The subsequent
integration runs used `LC_ALL=C`. No Rust/Clippy warnings were suppressed.

Independent review rechecked all reported fixes with no remaining findings in
its scope. This is source review of the working candidate, not exact-commit
approval to land. Main remains clean; this work is uncommitted, not installed,
and not merged. No live boards were changed. Linux CI and actual interactive
host behavior remain unqualified by these local tests.
