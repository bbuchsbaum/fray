# Mote authority integration gates

The 2026-10-06 local sweep preserves the acceptance contracts of
`bd-01M3RZ9BCSBBSS6WXRCSWQ33KV` (review/landing) and
`bd-01M3RZ9C15BSCYGHT1SYFBRV52` (dispatch/handoff). These features are not
complete. No mutating `fray land`, `accept`, or `handoff` is advertised.

## Upstream implementation review: `63454fe`

Mote commit `63454fe71036c8e0eecd7ee2db6e84f0b7ba29a1` adds the requested
single-store publication fence, actor-specific landing, and expected-holder/token
handoff. The historical source findings below describe the earlier `fd53f0a`
implementation. An independent review and additional executed reproducers at
the new exact commit identify three remaining integration gates:

1. **Landing journal I/O failure reports success.** Pause at
   `landing-confirmed-op` and remove write permission from the synthetic
   authority directory. The first call returns exit 0/`outcome:landed` with a
   permission error, while the durable active journal remains `Updated` and
   unrelated writers are blocked. Restoring permission and repeating the exact
   request recovers. `src/landing.rs:412` sets the in-memory phase before its
   write; the attempt-error branch at line 512 returns success based on that
   phase. Every attempt error must force nonzero `recovery_required`.
2. **Target moves after the last check but before confirmation.** Pause at
   `landing-updated`, CAS the synthetic Git ref back to its original preimage,
   away from the candidate, then resume.
   The first call returns exit 0/`landed` with `target_current:false`. The
   external ref is preserved. The last check is at `src/landing.rs:383`; result
   construction at line 296 ignores target movement for the exit status. This
   is a fresh-call outcome, not an intentional historical retry.
3. **Claim acquisition needs authority activation.** Ordinary `begin`/`claim`
   do not enable admission ordering. On a fresh store, a later-published but
   earlier-stamped claim replaces a previously accepted holder; an activated
   store's positive control retains the first holder and rejects the second.
   `src/authority.rs:267` retains filename ordering until activation. Fray needs
   an explicit activation primitive or activation before acquisition; it must
   not manufacture an unrelated landing/handoff to activate a store.

Hosted [CI run 37545250747](https://github.com/bbuchsbaum/mote/actions/runs/37545250747)
passed Linux stable and Rust 1.85, but macOS failed
`coord::a4_begin_race_exactly_one_succeeds`: both callers returned success.
The isolated local existing suites passed 37 checks, including that race on
this run; that local pass does not erase the hosted failure. Four additional
deterministic regression checks yielded three failures and one activated-store
positive-control pass. The corrected I/O writer probe reproduced its failure
again. [Raw evidence and fixtures](evidence/mote-63454fe-review/manifest.json)
retain the source identity, passing and failing logs, and exact test versions.
No original Mote source or live store was changed.

The executed findings and self-contained fixture were posted and read back in
[Mote #18's follow-up](https://github.com/bbuchsbaum/mote/issues/18#issuecomment-6027275544).

The new API also requires upgraded writers sharing one store and working local
POSIX locks. Landing supports a nonempty fast-forward to the immutable reviewed
candidate; a merge result requires its own candidate and reviews. Reservations
remain separate from claim handoff, and adoption retries need authoritative
readback. Fray's strict features remain blocked pending the findings above.

## Historical findings at `fd53f0a`

An independent read-only review inspected Mote 0.1.0's CLI and source at
`fd53f0a`. Binary/source parity was not established, and these are source
findings rather than executed race tests. Existing Fray adapter tests prove
attention and reconciliation, not atomic cross-system authority.

Upstream request: [Mote #18: fenced landing and holder-checked handoff](https://github.com/bbuchsbaum/mote/issues/18),
filed and read back on 2026-10-06 against upstream main
`fd53f0aee1dcb4410a3ce97153d42725752b5410`. The issue includes both source-reviewed
interleavings and proposed acceptance tests; it does not claim executed race
reproducers. Issue #14 covers out-of-band landing reconciliation, a distinct
contract from prospective authorization fencing.

### Git landing

Mote replay orders operation filenames (`src/repo.rs:153`,
`src/reducer.rs:95`). `candidate landed` checks phase and authorization in
Mote replay (`src/reducer.rs:5863`). Git's `update-ref NEW OLD` checks the Git
preimage alone. Consequently this schedule is possible:

```text
Fray reads authorization A as landable
Mote accepts revoke(A)
Git's old-OID CAS succeeds
Mote rejects the landed receipt
```

An additional reread, Fray mutex, Git ref lock, or stable mutation key does not
make those systems one transaction. A late earlier-stamped revoke can also
invalidate an apparent Mote landing retrospectively. Strong revocation safety
requires a stable authorization/fencing protocol in Mote respected by all
relevant writers. This sweep does not weaken the ticket to a best-effort merge.

If the owner later accepts a weaker checked workflow, it must validate full
candidate/policy/review/evidence state, immutable commit, repository binding,
target preimage, and the actor's authorization grantee membership. Mote's
`candidate show --actor` currently computes landability without that actor
(`src/cli.rs:2074`, `src/state.rs:1790`). Journal exact OIDs and receipts before
publication. After a Git update, rejected or unknown Mote confirmation must
return nonzero with `git_updated=true` and exact recovery evidence; it cannot
report an abort with no ref change or reset Git silently.

Discriminating future tests must inject revocation before the final read and
between the read and ref update, replay an earlier-stamped revoke afterward,
and refuse a landable candidate whose grantees exclude the actor.

### Claim and reservation handoff

Carrier reservations can stay continuously blocking through close/adopt while
their TTL remains live. Competing adoption and expiry remain visible failure
states. They do not provide atomic work ownership.

Mote's `handoff` rereads whichever claim is current and fills its token
(`src/cli.rs:9324`); claim replay accepts that CAS without checking the sender
(`src/reducer.rs:895`). A third party can become holder between Fray's precheck
and Mote's read, and have its claim transferred. An upstream expected-holder
and expected-token primitive is required for strict sender-only safety.

Under adopted Mote, Fray may serialize an acceptance attempt as transport
coordination, then call Mote outside its daemon lock and confirm ownership.
That attempt is not an authoritative claim. ACK never changes ownership.
Disappearance can mark attention stalled immediately, but ownership cannot be
requeued until release or expiry is confirmed. Current release cannot revoke
another actor's live claim without impersonation, which the adapter forbids.

Future acceptance tests need competing accepts, restart at every journal
step, competing adoption, lease expiry, and holder replacement between
precheck and handoff. No live store or other repository was changed to test
these source findings. Filing upstream #18 changed only the requested GitHub
issue, with no Mote source or live-store mutation in the upstream repository.
