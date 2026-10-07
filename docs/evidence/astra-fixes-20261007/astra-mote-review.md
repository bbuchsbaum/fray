# Mote corrective-change review

**Verdict: APPROVE** for `b114f4b731a9f6836116054df7084a7dff0e90e7`, reviewed against `a86803de31e890ac6e4331c9961705cabcde2fcb` in `/private/tmp/mote-fray-prerequisites`. Reviewer: `astra-fresh-review-20261006`. HEAD and clean tracked/untracked status were checked during this review.

No actionable correctness defect found in this bounded corrective change. The previous P1 finding is addressed prospectively. This does not repair stores whose journal was already overwritten, grant landing authority, or approve an as-yet-unfrozen Fray fix.

## Source assessment

`src/authority.rs:338` calls `recover_publication()` while the existing Writer still owns the exclusive lock, before line 339 can replace `publication.json`. Its initial prefix validation permits the matching linked pending operation but continues to reject unrelated unadmitted data. Recovery admits the original exact bytes and removes its journal before the second request is prepared. If recovery reports an error, `?` prevents publication of the second request.

`src/cli.rs:9399`, `:9424`, `:9427`, and `:9444` propagate compensation and optional-note publication failures. In particular, a failed optional note can no longer fall through to the announcement. Already-admitted claim/reservation work remains visible, and the caller receives failure rather than completion. The authority protocol documents those partial-work and recovery boundaries.

## Regression coverage

- `src/authority.rs:416`, `reused_writer_recovers_linked_publication_before_next_operation`, constructs the actual linked-but-unadmitted state using the production journal writer and lower-level publisher. It exercises the same retained Writer, checks exact admission order, removal of the publication journal, successful reducer replay with both notes, and acquisition by another writer. This directly discriminates the former overwrite failure rather than merely asserting a new helper was called.
- `src/authority.rs:440`, `failed_same_writer_recovery_preserves_original_journal`, makes recovery reject mismatched linked/journal bytes. It checks the specific recovery error, byte-for-byte preservation of the first journal, and absence of the second operation. This covers the refusal boundary, not only successful recovery.

The CLI propagation changes are small and directly inspected; the two new tests principally exercise the library recovery invariant. They do not independently inject an OS I/O failure through the full `begin --note --announce` command. The prior original-failure reproducer and current control-flow change support closure of that finding without another injection run.

## Validation evidence and limits

I inspected `mote-full.log` and its metadata: `cargo test --locked --offline --all-targets`, exit 0, 425 passed / 0 failed / 2 ignored across 43 test targets. Both new named regressions passed. I inspected `mote-required.log` and its metadata: the fail-fast fmt, Clippy with warnings denied, rustdoc with warnings denied, and diff-check command exited 0.

These are retained implementation-run results, not independently rerun tests. No fresh failure injection, library interposer, source edit, ticket transition, live-store mutation, installation, daemon restart, hosted CI, or publication was performed in this corrective review. No exhaustive power-loss guarantee is inferred. Fray's corrected commit remains pending a separate exact-SHA review.
