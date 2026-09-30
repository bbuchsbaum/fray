# Design: the Mote adapter contract

Status: revision 5 (2026-09-30), for re-review. Epic child 5, Mote
bd-01M3RZ9BW6K159Y3AAQR2NF99A. Children 1 (review and landing), 4 (git guard)
and 6 (dispatch and handoff) depend on it.

Revision 1 (86ac705) drew seven blocking objections from design review on
fray card #26. Each was reproduced against Mote source or scratch stores, and
each is addressed below, marked **[D1]** to **[D7]**. Revision 2 (399315f)
resolved all seven and drew one more objection, fray #42: a third party can
adopt an orphaned carrier reservation. It is addressed in 7.1 and 7.2,
marked **[D8]**.

## Problem

"Mote tracks the work; Fray carries the conversation" is currently only a
naming convention. Fray tags cards with `--ref mote:ID` and never talks to
Mote. So an agent can take a Fray lane over paths that another agent holds in
Mote, verdicts on the board never reach the candidate that Mote uses to decide
landing, and nothing in Mote (a reservation expiring, a candidate becoming
landable) reaches anyone's attention.

The adapter makes that division real without creating a second ledger:

- **Mote owns** work, claims, reservations, candidates, reviews and landing
  authority.
- **Fray owns** attention, meaning who needs to know what, and how fast, and
  the conversation, including verdicts as evidence.
- Fray never records a substitute for state Mote owns.

## Facts this contract rests on

These were read from the Mote source at 95379f0 (`mote 0.1.0`) and verified in
scratch stores on 2026-09-30. Citations are to `/Users/bbuchsbaum/code/rust/mote`.

- **Store discovery.** Mote tries `--store`, then `MOTE_STORE`, then walks up
  from the working directory to the first `.mote/` (`repo.rs:50-61`,
  `cli.rs:11949-11966`). Candidate commands also run `git` from the current
  directory and from the store's parent (`cli.rs:1934`, `2518-2521`), so the
  working directory matters for them.
- **Store identity.** `store_id` is in `.mote/FORMAT.json`. Among command
  outputs, only `events` and candidate output carry it.
- **Actor.** Mote tries `--actor`, then `MOTE_ACTOR`, then
  `.mote/local/actor`. The multi-session guard fires only when the actor came
  from the local file (`cli.rs:1527-1551`). On `events`, `--actor` filters the
  output to that actor (`cli.rs:11267-11270`).
- **Ops.** Each op is an immutable file. The reducer replays ops in filename
  (timestamp) order, so the order decides acceptance. Verified: after an
  accepted reserve, copying in a competing op with an earlier timestamp
  transferred the reservation. The already-emitted `reservation.opened`
  vanished, and `events --after CURSOR` never showed the new op.
- **Exit codes.** 0 ok, 1 internal, 3 invalid, 4 store or duplicate error.
  Exit 2 covers both a reducer rejection and a command-line usage error
  (`main.rs:26-53`). For `preflight`, exit 2 means "conflicts found": a
  result, not a failure.
- **Output.**
  - `reserve` prints JSON even when rejected.
  - `unreserve`, `claim` and `release` print nothing and write `rejected: …`
    to stderr.
  - `handoff` writes `handoff claim rejected: …`.
- **Idempotency.** Keys are required for `candidate review` and supported for
  messages and roles. There are none for reserve, unreserve, claim, release,
  begin or handoff. A retried reserve is rejected as a same-actor duplicate.
  A candidate-review retry counts as idempotent only when verdict, body,
  evidence and `--expect` are all byte-identical (`cli.rs:2669-2697`).
- **Events.**
  - The cursor is the op id. Event types are op-derived (for example
    `claim.acquired`, `reservation.opened`, `candidate.*`) plus a few derived
    ones (`reservation.expiring` and `.expired`, `presence.*`,
    `request.stale`). There is no "landable", "review requested" or "handoff"
    event.
  - The cost grows quadratically with store size (`events.rs:461-466`,
    `660-670`). On a copy of a 1,953-op store: a full run took 29.1 s; from
    the tail, 6.5 s; from the tail with `--kind`, 1.2 s.
- **Reservations.**
  - TTL defaults to 3,600 s, and `reserve --ttl` overrides it.
  - A live reservation cannot be renewed: re-reserving is rejected as a
    duplicate.
  - An orphaned reservation, whose issue is closed or deleted, is still live
    and still blocks others (`reducer.rs:3513-3530`).
  - `adopt --issue WORK RV [--ttl]` re-homes an orphan onto any open issue
    the adopter has claimed, with a fresh TTL (`state.rs:869-912`). Without
    `--ttl` the lease resets to the 3,600 s default. Nothing ties the adopter
    to the orphan's previous holder: any actor holding any live claim can
    adopt it (`reducer.rs:3640-3700`, verified with a third party). Mote's own
    skill tells agents to adopt orphans, and `mote audit` lists them.
  - Anyone can close an issue, including a carrier claimed by someone else.
    So a deliberate actor can still close an open carrier and adopt its
    reservation. The adapter detects this through confirmation and
    reconciliation; it cannot prevent it.
- **Where state lives in JSON output** (verified; fray #43).
  - **Claims.** `show --json` and `ls --json` carry no claim. The live
    holder is in `board --json` under `.active_claims[].claimed_by`. The op
    that produced a claim is the last accepted `kind=claim` entry of `mote
    history --json ID`, or the `op_id` of its `claim.acquired` event.
  - **Reservations.** `who-has` and `board` give no clock, and a reservation
    keeps its id across adoptions. Every adopt resets `lease_until_ts`, so
    holder plus lease identifies a state. Reservation events
    (`reservation.opened`, `.adopted`) carry `ttl_s`, not `lease_until_ts`.
    The lease is `event.ts + data.ttl_s`, which matches `who-has` exactly
    (`reducer.rs:1047-1052`), and the holder is `event.actor`.
  - **Candidates.** `candidate show --json` gives `.phase.op_id` and
    `.policy.op_id`. `reviews` is an object keyed by reviewer
    (`reviews.<name>.op_id`). `authorization` is null until granted, then
    carries `op_id`. Each `evidence[]` entry has `op_id`. Reviews, evidence,
    authorization and policy amendments (`amend-reviewers`) all change
    landability without a new phase op.
  - `who-has` replays the store on every call, so it is accurate when it
    runs. It is not final: an op stamped earlier but published later can
    still change the answer.
  - Paths are case-sensitive and literal, `.` is rejected, and `Src/a.rs` does
    not overlap `src/`.
- **Handoff.**
  - `mote handoff` transfers the claim, filling in the current compare-and-set
    token, and with `--release` closes the sender's reservations.
  - The reducer does not check who is sending (`cli.rs:9347-9352`,
    `reducer.rs:881-910`): any actor can hand off anyone's claim.
- **Candidates.**
  - A review is per reviewer (approve, block or comment), compare-and-set on
    the reviewer's previous op.
  - Only named reviewers, or holders of an eligible role, may review
    (`reducer.rs:5183-5194`), and the proposer may not (`reducer.rs:5121`).
  - Supersede carries no reviews forward.
  - `candidate show --json` lists the blocking `landability` reasons.

## Contract

### 1. Transport

- Fray runs the `mote` binary and parses `--json` output. The one exception
  is a read-only read of `FORMAT.json` for the store id.
- Every call passes `--store` and `--json` explicitly. Every call except
  `events` also passes `--actor` [D1].
- Calls have bounded timeouts: 10 s for reads, 30 s for mutations. On timeout
  the adapter kills and reaps Mote's process group before any re-read.
- The adapter supports `mote 0.1.x`. It checks `mote --version` once per
  process and refuses anything else with `mote_version`.
- The adapter lives in the client. It never runs in the daemon or under the
  store lock that serializes every agent's requests.
- Outcomes are classified from the exit code, stderr and JSON together. A
  usage error (exit 2 without `rejected`) is `mote_invalid`, an adapter bug,
  not a rejection. A `preflight` exit 2 is its result.

### 2. Which store [D6]

The adapter binds the store to the board with the same resolution that picks
the board (`client::home`), and records the binding:

1. If `MOTE_STORE` is set, that store is used.
2. If the board is `<root>/.fray` (an ancestor `.fray/`), the store is
   `<root>/.mote`.
3. If the board is `<git-common-dir>/fray`, the store is `.mote/` in the main
   worktree, taken from the first entry of `git worktree list --porcelain`.
4. For a bare repository, or a submodule (a `.git` file whose gitdir sits
   under another repository's `modules/`), `MOTE_STORE` is required.

On first use the board records the store path and its `store_id` (read from
`FORMAT.json`) in `meta` (`mote_store`, `mote_store_id`). Every later call
re-reads `FORMAT.json` and compares. A mismatch fails with
`mote_store_mismatch`, naming both ids; nothing falls back silently. No
`.mote/` means no adapter, and Fray behaves as it does today.

Candidate commands run with the agent's own worktree as their working
directory, because Mote runs git there.

### 3. Which actor

The Mote actor is the Fray identity name, passed with `--actor`, so Mote's
local-file guard cannot misattribute a write. Fray already refuses a second
live session under one name, so name to actor is one-to-one. That is
collision prevention, not authentication.

A mutation with no Fray identity is refused. If `MOTE_ACTOR` in the
environment differs from the Fray name, `brief` warns: manual `mote` calls
would otherwise act as a second actor whose reservations conflict with the
agent's own. The owner never acts in Mote through the adapter.

### 4. Reads and mutations fail differently

| Call | Examples | On failure |
|---|---|---|
| Invalid (exit 3, or exit 2 without `rejected`) | any | `mote_invalid`: an adapter bug or version mismatch, reported with Mote's message. Record nothing. |
| Read | `who-has`, `preflight`, `show`, `candidate show`, `events` | Degrade to advisory: continue with Fray's own view, printing one warning that names the command and error. Never block a commit or a message. |
| Mutation, rejected | `reserve`, `unreserve`, `claim`, `adopt`, `handoff` | Fail with `mote_rejected` and Mote's reason. Record nothing that Mote owns. |
| Mutation, unconfirmed (timeout, exit 1 or 4, unparseable output) | same | Re-read the outcome (see 5). If that also fails, fail with `mote_unconfirmed` and name the read to run. Never report success. |

Candidate reviews follow the rule in 7.3.

### 5. Idempotency without Mote keys

- **Keyed ops (candidate review, messages).** The adapter stores the exact
  payload it sent in Fray's request record: verdict, body, evidence, the
  `--expect` token and the key. A retry replays that payload byte for byte,
  never re-reading the token [D7].
- **reserve.** Before writing, the adapter runs one `preflight --issue ISSUE
  --paths …` call. A `same_actor_duplicate` on the same issue and paths means
  the reservation is already present, so it is reported as present, not
  created again.
- **claim and unreserve.** A retry is naturally safe.
- **handoff.** Before sending, and before any retry, the adapter reads
  `board --json` (`.active_claims[].claimed_by` for the work issue) and
  proceeds only while the sender still holds the claim. If the intended
  recipient already holds it, the handoff is done.

### 6. Mote to Fray attention [D1, D2, D3]

- **Reading events.** A sync runs `mote events --json --after CURSOR --kind
  K…` with no `--actor`, restricted to the event types below. Fray decides the
  recipients. On adoption the cursor starts at the current tail, so history
  is never replayed.
- **Dedup and cursor.** The board keeps a table `mote_events(store_id,
  event_id, recipient)` with that composite primary key, recording the card
  produced for each recipient. One event can therefore reach several agents.
  The cursor (`meta.mote_cursor`) only moves forward: the store applies a
  compare-and-set to it in the same transaction as the cards, so a
  concurrent or slower sync can never move it back.
- **Where sync runs.** Sync is never on the `brief` or hook critical path.
  `fray mote sync` runs on demand. A drive or watch runner runs it in the
  background at most once a minute, and a timed-out sync leaves the cursor
  where it was. After three consecutive timeouts, the cursor is reseeded at
  the tail; the gap is covered by reconciliation. `--after` compares op ids
  as strings (`events.rs:1377-1391`), so a timestamp-shaped cursor is valid.
  A latency test on a store of at least 2,000 ops bounds one sync from the
  tail. The quadratic `events` cost is also filed with Mote
  (see Requests).
- **Which events produce attention, and for whom.**

  | Mote event | Attention | Recipient |
  |---|---|---|
  | `reservation.expiring`, `reservation.expired` | "your reservation on PATHS is expiring" | the holder |
  | `claim.acquired`, where the new holder differs from the previous holder | handoff received | the new holder, and the previous holder as a receipt |
  | `candidate.*` (proposed, reviewed, evidence, authorized, superseded, landed) | Re-read `candidate show`. Report a change in landability or blocking reasons; a named reviewer who has not reviewed is asked for review. | the proposer, named reviewers, the authorizer |

  "Landable" and "review requested" are derived from `candidate show`, never
  assumed from the event name.
- **Reconciliation.** Because the feed can skip a late op and an emitted op
  can later be rejected, every sync also reconciles a bounded snapshot: the
  agent's claims and reservations (`mote ls`, `who-has`), and the candidates
  where the agent is proposer, named reviewer or authorizer (`candidate list`
  and `candidate show`), against the attention cards it holds. A wrong card
  is superseded with a note. A missing one is created. The event path
  and reconciliation key a state the same way, so their cards dedupe against
  each other, and a state that recurs (A, then B, then A) is still reported,
  because its key differs:

  | Entity | State key | Source |
  |---|---|---|
  | Claim | `claim:<issue>:<op_id>` | last accepted `kind=claim` entry of `mote history --json`, or the `claim.acquired` event's `op_id` |
  | Reservation | `rv:<rv>:<actor>:<lease_until_ts>:<phase>`, where phase is `expiring` or `expired` | `who-has --json`; from an event, `event.actor` and `event.ts + data.ttl_s`. Each adopt resets the lease. |
  | Candidate | `cand:<id>:<phase.op_id>:<latest op>:<landability hash>` | `candidate show --json`. `latest op` is the greatest op id across `policy.op_id`, `reviews.<name>.op_id`, `authorization.op_id` and `evidence[].op_id`. `landability hash` is a short hash of `landability.landable` plus the sorted `reason_codes`, so that any change in landability produces a new key, whatever op caused it. |
- **Who hears that a claim changed hands** (implemented in slice 5b). The
  board keeps the last holder it has seen for each entity (`mote_claims`). It
  is seeded from `board --json` on the first sync, and every claim event then
  updates it in the same transaction as the cursor.
  - The new holder hears of a handoff: `claim.acquired` where `to` is not
    the actor.
  - The previous holder hears when someone else moved their claim: a
    third-party handoff, or taking over a claim that had expired. They do
    not hear when they handed it off themselves.
  - Both cards share the key `claim:<entity>:<op_id>`, one per recipient.
  - Transitions apply once, in op order: one whose op is not newer than the
    op stored for that entity is skipped. So a replayed chunk (from an
    interrupted or concurrent sync) cannot compare an old transition with a
    newer holder. Seeded holders record the seed cursor, which sorts below
    every later op.
- **Bounded cards.** Titles and summaries are clipped to the card limits. An
  item that is still invalid is skipped and reported, never allowed to stall
  the cursor for every later event.
- **Trust.** Cards from Mote are authored by the reserved identity `mote`.
  Names that fold to it (such as `Mote`) cannot join. Any joined agent can
  call `mote_ingest`: like the rest of Fray, this guards against collision,
  not against a hostile process running as the same OS user.
- **Clock skew.** An op stamped in the future sorts after later-arriving ops.
  The cursor may then skip them, and reconciliation is what catches them.
- **Mote ownership in cards.** A Fray card produced from Mote carries
  `mote:ID` and says that Mote holds the state. Resolving the card never
  changes Mote.

### 7. What each dependent child gets

#### 7.1 Lanes and the git guard (child 4) [D5]

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
  makes gap-free handoff possible (7.3). Carriers are closed when the lane is
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

#### 7.2 Handoff (child 6) [D4]

Revision 1 said a gap-free handoff was impossible on Mote 0.1.0. That was
wrong. Carriers make it possible:

1. Check that the sender holds the work claim (5); refuse otherwise. This
   compensates for Mote's missing holder check.
2. Post the Fray handoff packet: state, next step, evidence, lanes, and the
   carrier and reservation ids.
3. `mote handoff WORK --to RECIPIENT` transfers the work claim. The carriers
   stay open, so the reservations are not orphans and cannot be adopted by
   anyone.
4. On the recipient's `accept`, one client call does, for each carrier, back
   to back:
   - close the sender's carrier (anyone may close an issue);
   - immediately `mote adopt --issue WORK --ttl T RV`;
   - confirm the holder with `who-has`.

   A reservation is orphaned only between those two calls, not for the hours
   before the recipient accepts [D8]. If a third party took it in that
   interval, both agents get urgent attention naming the holder, and the lane
   is marked lost, not held.
5. The recipient may later move the reservations to its own carriers by the
   renewal step in 7.1.

The paths are never unreserved. Each step is checked before it runs, and
`fray handoff --resume` continues from the first incomplete step. Until
recipients accept, the sender's open carriers keep the paths reserved in the
sender's name. The handoff packet says so.

#### 7.3 Review and landing (child 1) [D7]

- The Fray verdict is always recorded on the board. It is conversation and
  evidence, open to any reviewer, and bound to its SHA or manifest.
- It is mirrored to Mote as a candidate review only when three conditions all
  hold: the reviewer is a named reviewer or holds an eligible role, the Fray
  verdict's SHA equals the candidate's `commit_oid`, and the reviewer is not
  the proposer. Approve maps to approve; object or blocked maps to block.
- A verdict that is not mirrored says why ("not a named reviewer", "reviewed
  SHA differs from the candidate") and changes nothing in Mote.
- Landability always comes from `candidate show`, never from Fray verdicts.

## Requests to Mote

These are not filed in the Mote repository. Filing there would affect
another project, which the charter reserves for the owner.

1. `events` cost that is linear in the ops read, not quadratic in the size of
   the store.
2. A holder check on `handoff`: only the claim holder, or an authorized role,
   can hand off. Likewise on `adopt`: only the orphan's previous holder, or
   whoever now holds the claim on the orphan's work issue, can adopt it.
3. Renewal of a live reservation, and transfer of reservations with the
   claim on handoff.
4. Idempotency keys on reserve, unreserve, claim, release, begin, adopt and
   handoff, and JSON output for the commands that have none.
5. A stable acceptance order, so an accepted op cannot be overturned
   retroactively by a late op with an earlier timestamp. Until then, Fray
   re-reads Mote wherever it matters: `fray land`, the pre-push guard, and
   reconciliation.
6. Distinct exit codes for usage errors and reducer rejections.

## Acceptance

Tests use a stub `mote` on `PATH` for failure injection (it records its argv
and returns canned output and exit codes), plus the real binary in temporary
stores. They cover:

- with and without `.mote/`;
- store binding: an ancestor `.fray`, a git-common-dir board, a worktree
  outside the checkout, a bare repository and a submodule (both require
  `MOTE_STORE`), and a `store_id` mismatch refused;
- a read outage that warns, and a mutation outage that fails loudly and
  records nothing Mote owns;
- multi-agent routing, where one event reaches every recipient exactly once,
  and the cursor never moves back under concurrent syncs;
- a late-op flip caught by reconciliation;
- latency: one sync from the tail on a store of at least 2,000 ops, within
  the read timeout;
- path translation: case, directory versus file, and refusal of glob and
  whole-repository lanes;
- lane TTL warning and gap-free renewal, and compensation when Fray refuses
  after Mote accepted;
- handoff: refused from a non-holder, gap-free in the carrier-and-adopt flow,
  and resumed after an interruption at each step;
- a third party adopting in the close-and-adopt interval, which is detected
  and reported as lost, never shown as held;
- adopt always passes `--ttl`, so the lease is not reset to the default;
- a verdict from an unnamed reviewer, and one for a mismatched SHA: both
  recorded on the board, neither mirrored, with the reason shown;
- a keyed review retried after a timeout, replayed byte-identically;
- a candidate that becomes landable within one phase, reported once in each
  of three cases: a review arrives, evidence arrives, and `amend-reviewers`
  removes a missing reviewer;
- the handoff holder check reading `board --json`, including an unclaimed
  work issue.
