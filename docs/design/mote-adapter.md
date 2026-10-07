# Design: the Mote adapter contract

Status: revision 6, locally qualified on 2026-10-06. Mote prerequisites are
`a86803de31e890ac6e4331c9961705cabcde2fcb`; the package version alone does not
identify these capabilities. See [validation](../VALIDATION.md) and the
[historical upstream findings](../MOTE_CAPABILITY_GAPS.md).

Mote owns work, claims, reservations, candidates, reviews and landing authority.
Fray owns requests, routing, attention and recoverable client operations. A Fray
receipt, ACK, offer, card assignment or attempt lock never grants Mote ownership.

## Transport, store and actor

Fray runs the public Mote CLI outside daemon transactions. It reads only
`FORMAT.json` directly, for store identity. Every call passes `--store` and
`--json`; all except `events` pass the Fray actor explicitly. Events are JSON
lines and their `--kind` accepts categories such as `claim,reservation,candidate`,
not individual event types. Reads default to 10 seconds, mutations to 30 seconds;
timeout stops and reaps the subprocess group before recovery reads.

Store discovery follows the chosen Fray board: `MOTE_STORE`, a sibling `.mote/`
of the governing ancestor `.fray/`, or the main worktree's `.mote/` for the Git
common-directory board. Bare repositories, submodules and unrelated explicit
homes require `MOTE_STORE`. The bound store id is checked before every call;
replacement at the same path is refused. Candidate operations retain their
original canonical working directory. Local target-evidence attention probes
the canonical store parent, matching Mote's target-scope command.

The Fray name is the Mote actor. Strict mutations require a joined non-owner
identity. No absent actor is impersonated to revoke its claim. Without a paired
Mote store, existing Fray cards and standalone review evidence remain advisory;
strict candidate/dispatch commands refuse. Ordinary advisory reads may degrade
with a warning; authority-critical reads fail closed.

Nonzero structured receipts retain exit status, complete JSON, Git OIDs,
journal state and current holder/token. They never confirm successful landing
or review. A timeout retains the exact request for readback/retry, without
claiming that buffered output was recovered.

## Upgrade and authority boundary

Strict commands require `mote.authority-status.v1`, the bound store id, enabled
authority version 1, a nonempty genesis digest, `stable_claim_order`, and the
operation's advertised capability (`holder_checked_handoff` or
`checked_landing_results`). Reads never activate authority. Fray's daemon must
advertise `mote_workflows`, `mote_admission_order` and `dispatch` as applicable.

All writers sharing a store must use the upgraded protocol on a filesystem
with working local POSIX locks. After upgrading them, an operator may explicitly
run:

```sh
fray --key enable-v1 mote authority-enable --all-writers-upgraded
```

This is a migration assertion, not automatic detection of every writer.
Qualified Mote `claim` and `begin` also activate ordering before acquisition.
No original store was activated or global binary installed by this sweep.

## Exact operation journal

Strict client operations require a caller key. Fray durably saves immutable
arguments, store id and cwd before mutation, then checkpoints complete outcomes
and readbacks. A kernel actor/key lock serializes transport across process death
without retaining the daemon mutex or an expiring coordination lease. The daemon
uses revision CAS for journal writes; different arguments under one key refuse.

```sh
fray operation show KEY
fray operation resume KEY
fray handoff --resume KEY
```

Inspection works without a reachable Mote binary. Resume uses the saved request,
never a freshly read CAS token. Pending journals are retained through compaction.
A completed retry is a historical receipt accompanied by current observations
where available; it is not renewed authority or a renewed lease.

## Admission-ordered attention

Legacy stores retain filename cursors and their historical three-timeout reseed.
For enabled authority, Fray binds `admission_v1` to the genesis digest and uses
Mote's returned event order. Raw ids are anchors, not lexical timestamps.
Cursor and revision CAS, claim-event identity deduplication and card writes commit
together. Snapshot reconciliation has separate claim state, so it cannot invent
reverse feed transitions when it runs ahead of the feed.

The first admitted sync quietly seeds both claim views from a filtered public
history and stores the last raw op id; an empty seed has an initialized nullable
anchor. Rebinding a legacy cursor re-baselines without replaying historical
handoffs, including transitions since its last legacy sync. Subsequent timeouts
leave the exact admitted cursor unchanged; no UTC tail is substituted.

Reservation projections are read separately with `--kind reservation`, then
filtered to expiring/expired attention. They never advance the raw anchor; a
future-stamped raw operation cannot hide a due warning. Candidate state changes
are delivered to their relevant participants, and unchanged states stay quiet.
Unknown recipients are reported rather than silently treated as notified.

`fray mote sync` runs on demand, and existing drive/watch clients run it in the
background. Dispatch reconciliation also runs there for admitted stores. It
requires an active client; the daemon neither polls nor executes Mote.

## Immutable candidate review and fenced local landing

```sh
fray --key request-1 review candidate CANDIDATE --to reader --title 'Review' 'Check behavior'
fray --key verdict-1 review candidate-verdict CARD object --at git:OID --expect 1 'Evidence'
fray --key successor-1 review successor CARD --candidate NEXT --expect 1
fray --key approval-1 review candidate-verdict NEXT_CARD approve --at git:NEXT_OID --expect 1 'Verified'
fray land CANDIDATE --target main --check
fray --key landing-1 land CANDIDATE --target main --before OLD_OID
```

The explicit candidate command freezes store/candidate/commit identity. Its
subject cannot be edited to another SHA. `review successor` requires Mote's exact
supersession relationship, creates a fresh review for the original addressee,
carries open objections with their original objector's closure rights, and marks
old approvals stale. Participants hear the successor; Mote sync requests its
named reviewers according to current policy. No approval is copied.

Proposer self-review refuses. Named-reviewer/role eligibility and quorum remain
Mote policy. Fray maps approve to approve, object/blocked to block and submits
Mote's exact keyed review CAS. A rejection or uncertain exit records no Fray
verdict. A successful saved receipt can later be historical; it does not overwrite
an authoritative replacement review or create a fresh objection. Legacy
`review request --ref mote:...` and standalone SHA/manifest reviews remain
conversation evidence, explicitly advisory.

A changed local target produces separate advisory attention to the proposer and
evidence producer. It says the local target differs from the recorded scope,
with old/new OIDs; it does not change Mote evidence, landability, approvals or
wake reviewers again. The probe checks receipt identity/binding, object format,
full branch ref and object readability. It does not prove Mote's BLAKE3 repository
identity. Missing refs/objects produce no stale-evidence assertion. Refreshed
matching target evidence clears the mismatch state.

Landing delegates entirely to `mote candidate land`: a nonempty local fast-forward
to the immutable reviewed candidate. An arbitrary merge result needs its own
candidate/reviews. There is no push. The read-only check observes eligibility and
matching target evidence; the mutation revalidates authorization, preimage,
repository and Git scope under Mote's publication fence. Nonzero or unknown
confirmation retains the full recovery receipt, including whether Git changed;
Fray never silently resets Git. Exit 0 is additionally checked against the exact
candidate, actor, key, old/new OIDs and current Git/Mote readbacks.

## Dispatch and recoverable carrier handoff

```sh
fray --key offer-1 send --to anyone-free 'Bounded work' --mote ISSUE --tag rust --offer-ttl 300 --claim-ttl 300
fray --key accept-1 accept CARD --expect 1
fray --key progress-1 accept CARD --expect 1 --progress 'Started validation'
fray dispatch sync
fray --key transfer-1 handoff CARD --to peer --state 'Half done' --next 'Run checks' --evidence REF --carrier CARRIER=RV --reservation-ttl 28800
fray --key adopt-1 accept HANDOFF_CARD
```

Routing chooses an enabled live idle/waiting peer matching the tag, excluding the
sender, reserved identities, muted cards and a fresh busy terminal. No eligible
peer produces a durable visible status. An offer has a bounded attention expiry;
it routes to the next untried peer or returns to the sender.

Explicit accept serializes one attempt by offer generation and actor/key/session.
Later accepts name the pending/current winner. The client then acquires Mote's
finite claim outside the daemon lock, confirms holder/token with a bounded
history/board/history stability check, and freezes the exact confirmation RPC
before submission. Retry after renewal/expiry does not reacquire an old claim.
The Fray task has an assignee but no substitute Fray ownership lease.

Disappearance before progress immediately marks stalled and notifies the sender.
Requeue waits for observed Mote release/expiry, a bounded uncertain-acquisition
window and an idle attempt lock. Reconciliation locks before its claim read and
CAS-checks attempt identity as well as generation. External renewed/foreign live
claims delay routing; progress prevents automatic requeue. Observations can
become outdated after a read: the next Mote claim remains acquisition authority.
Terminal cards cannot be reopened by a delayed confirmation.

Handoff first saves an addressed structured packet, then sends Mote the exact
expected holder/token and key without `--release`. The source must have one Mote
work reference. Existing carrier reservations retain their id and live path set;
a carrier must differ from the work issue because acceptance closes carriers.
After confirmed work transfer, the recipient accepts each carrier by closing it,
reading its orphan clock, adopting with that exact clock and an explicit TTL,
then checking ownership/path continuity. The client never unreserves these paths.
An interrupted close/adopt resumes from saved observations; already adopted
reservations are not renewed. Final work and reservation readbacks precede
completion. Older sender retries cannot erase completed adoption evidence.

Continuity holds only while the carrier lease stays live. A competing adoption,
expiry or changed path set reports loss, with urgent attention for both parties,
and never reports the paths held. This is a recoverable sequence, not atomic
claim-plus-reservation transfer. Adoption need not be exclusive to the intended
recipient in Mote; the confirmation boundary detects that loss.

## Existing lane and guard contract

- **Issue rule.** Under Mote, a lane needs exactly one `mote:ISSUE` reference
  on its card (`--for CARD`) or given as `--mote ISSUE`. Zero or several is
  refused, with a message naming the choice.
- **Path rule.** Lane paths go through Fray's normalizer first. A directory
  becomes `dir/` with a trailing slash; a file stays literal. Whole-repository
  lanes (`.` or `*`) and glob lanes are refused under Mote, because Mote
  cannot represent them. Fray's case-insensitive overlap stays in force for
  Fray's own warnings. Mote is authoritative only for the literal paths it
  holds.
- **Carrier issue.** The reservation is not made on the work issue itself. It
  is made on a lane carrier: a Mote bead titled `lane: PATHS`, tagged
  `fray-lane`, and related as a child of the work issue. It is created with
  `mote new --id fray-lane-<request key>` (the `bd-` prefix is reserved), so
  a retried creation is refused as already existing rather than duplicated. The carrier is what
  makes gap-free handoff possible (carrier handoff above). Carriers are closed when the lane is
  released.
- **Ordering and compensation.** `fray lane take` runs in the client:
  1. Create the carrier and claim it.
  2. `mote reserve --issue CARRIER --ttl T`.
  3. Take the Fray lane.

  If Mote refuses, there is no lane and the carrier is closed. If Fray
  refuses after Mote accepted, the client compensates with `mote unreserve`
  and closes the carrier; if compensation fails, it reports `mote_unconfirmed`
  with the ids. Queue promotion happens in the daemon and cannot call Mote.
  So a promoted lane is marked `awaiting_mote`, and the promoted agent's
  notice gives the one command that completes steps 1 and 2 (`fray lane take
  --resume LANE`).
- **TTL.** `T` defaults to 8 hours, beyond Fray's 4-hour stale-lane horizon.
  Mote cannot renew a live reservation. At 80 % of `T`, the holder is told,
  and `fray lane renew` re-homes the reservation with a fresh TTL. It claims
  a new carrier, then closes the old one and immediately runs `adopt --issue
  NEW --ttl T RV`, back to back in one client call. The paths are never
  unreserved, and the reservation is an orphan only for the interval between
  those two calls [D8]. A Mote renew verb is requested.
- **Confirm every adopt [D8].** After each adopt, the adapter confirms with
  `who-has` that the actor holds the reservation. If a third party adopted it
  in the interval, the adapter raises urgent attention naming the new holder
  and the paths. The lane shows `lost_to AGENT` and is not reported as held.
- **Guard.** The git guard checks the staged or pushed paths with one
  `preflight --issue ISSUE --paths …` call per hook invocation, never one
  call per path.

## Qualification and remaining requests

Always-run tests include synthetic public-CLI failure/feed courts. Separate
scripts take an explicitly qualified Mote binary and create disposable Git/Mote/
Fray fixtures: `scripts/mote_workflow_integration.py` and
`scripts/dispatch_integration.py`. The retained evidence distinguishes those real
CLI courts from synthetic ownership transitions and the earlier two-hour soak.

[Mote #18](https://github.com/bbuchsbaum/mote/issues/18) tracks the published
upstream request. Local fixes and local checks do not establish hosted CI or
publication. Reservation renewal/atomic transfer and keys on unkeyed claim/adopt
remain possible upstream improvements; this integration does not require them
by pretending those mutations are idempotent. Paid head-to-head comparison,
actual native-host trials and the parked MCP shim remain outside this work.
