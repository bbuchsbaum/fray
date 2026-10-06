# Authority capabilities still required from Mote

The 2026-10-06 local sweep preserves the acceptance contracts of
`bd-01M3RZ9BCSBBSS6WXRCSWQ33KV` (review/landing) and
`bd-01M3RZ9C15BSCYGHT1SYFBRV52` (dispatch/handoff). These features are not
complete. No mutating `fray land`, `accept`, or `handoff` is advertised.

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

## Git landing

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

## Claim and reservation handoff

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
