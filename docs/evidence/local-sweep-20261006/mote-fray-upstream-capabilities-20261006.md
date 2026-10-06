Fray's strict review/landing and dispatch/handoff features need Mote to validate authority at the mutation boundary. A caller-side precheck followed by the current CLI operations cannot establish those guarantees. This request covers the two missing contracts; they can be split into separate implementation issues if preferable.

Source reviewed: Mote 0.1.0 at [`fd53f0aee1dcb4410a3ce97153d42725752b5410`](https://github.com/bbuchsbaum/mote/tree/fd53f0aee1dcb4410a3ce97153d42725752b5410), which is also current upstream `main` as checked on 2026-10-06. These are source-reviewed interleavings and proposed acceptance tests, not claims that an executed race reproducer has already demonstrated them.

### 1. Candidate landing with authorization fencing

The candidate landed receipt validates authorization during Mote replay, while Git's old-OID CAS validates only the ref preimage. This permits the following schedule:

```text
Fray reads authorization A as landable
Mote accepts revoke(A)
Git update-ref NEW OLD succeeds
Mote rejects the landed receipt
```

An extra reread or stable mutation key does not close the gap between the final read and Git update. If revocation wins the authority boundary, Git remains unchanged; if landing wins, later publication of an earlier-stamped operation cannot retroactively revoke that committed authorization. Interrupted operations expose explicit recovery state.

Relevant source: [operation ordering](https://github.com/bbuchsbaum/mote/blob/fd53f0aee1dcb4410a3ce97153d42725752b5410/src/repo.rs#L153), [replay ordering](https://github.com/bbuchsbaum/mote/blob/fd53f0aee1dcb4410a3ce97153d42725752b5410/src/reducer.rs#L95), [landed receipt validation](https://github.com/bbuchsbaum/mote/blob/fd53f0aee1dcb4410a3ce97153d42725752b5410/src/reducer.rs#L5863), and [candidate view](https://github.com/bbuchsbaum/mote/blob/fd53f0aee1dcb4410a3ce97153d42725752b5410/src/state.rs#L1790). The CLI's candidate-show view also does not pass the selected actor into its landability computation ([caller](https://github.com/bbuchsbaum/mote/blob/fd53f0aee1dcb4410a3ce97153d42725752b5410/src/cli.rs#L2074)); a displayed landable flag is not sufficient proof that the caller is an authorization grantee.

Requested contract:

- A Mote-authoritative primitive or prepare/commit protocol that fences landing against revocation through the Git update, respected by all relevant Mote writers.
- Bind the candidate, immutable commit, repository, target ref/preimage, policy/review/evidence state, and authorized actor.
- Make actor-specific landability explicit.
- If Git changed but Mote confirmation is rejected or unknown, report nonzero with `git_updated=true`, exact old/new/current OIDs and durable recovery evidence. Never silently reset Git or report an unchanged-ref abort.

Acceptance tests should inject revocation before the final read and between that read and ref update, replay a late earlier-stamped revoke, reject a caller outside the grantee set, and exercise restart/retry at every journal boundary.

Related: #14 requests honest recording of an out-of-band landing. This request asks for the stronger prospective authorization contract; an honest after-the-fact receipt alone cannot fence the Git mutation.

### 2. Handoff with caller-supplied holder/token preconditions

The CLI rereads whichever claim is current and supplies its token ([handoff caller](https://github.com/bbuchsbaum/mote/blob/fd53f0aee1dcb4410a3ce97153d42725752b5410/src/cli.rs#L9324)); replay accepts the claim CAS without checking that the sender is the current holder ([claim transfer validation](https://github.com/bbuchsbaum/mote/blob/fd53f0aee1dcb4410a3ce97153d42725752b5410/src/reducer.rs#L895)). A stale caller can therefore target a replacement claim rather than the claim it intended to transfer:

```text
Alice/Fray observes Alice holding claim T1
Alice's lease expires; Bob obtains replacement claim T2
Alice invokes handoff to Carol
CLI rereads Bob/T2 and fills T2 as its CAS token
Bob's replacement claim can be transferred by Alice's operation
```

Requested contract:

- Accept the holder and token expected by the caller; do not substitute the latest token after a stale precheck.
- Validate those preconditions and sender authority at the accepted mutation boundary. Ordinary handoff must require the current holder; coordinator/owner release or revocation, if supported, needs an explicit authorized operation rather than actor impersonation.
- Provide idempotency keys for handoff and byte-identical retry semantics, and return explicit conflict/expiry/partial outcomes. Carrier reservation adoption must preserve protection only while its TTL remains live; transfer, expiry and competing adoption must not be reported as atomic ownership success.

Acceptance tests should replace the holder between precheck and handoff, attempt transfer by a nonholder, race competing accepts/adoptions, expire leases between journal steps, and restart/retry at every step. Fray can coordinate transport attempts, but it must not invent authoritative ownership or treat message ACK as a transfer.

Until these contracts exist, Fray's two features remain explicitly blocked rather than being presented as best-effort implementations of strict guarantees.
