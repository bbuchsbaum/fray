# Design: the Mote adapter contract

Status: contract for review (2026-09-30). Epic child 5, Mote
bd-01M3RZ9BW6K159Y3AAQR2NF99A. Children 1 (review and landing), 4 (git guard)
and 6 (dispatch and handoff) depend on it.

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
- **Fray owns** attention, meaning who needs to know what, and how fast.
- Fray never records a substitute for state Mote owns.

## Facts this contract rests on

These were read from the Mote source at 95379f0 (`mote 0.1.0`) on 2026-09-30.
Citations are to `/Users/bbuchsbaum/code/rust/mote`.

- **Store discovery.** Mote tries `--store`, then `MOTE_STORE`, then walks up
  from the working directory to the first `.mote/` (`repo.rs:50-61`,
  `cli.rs:11949-11966`). It never consults Git. A worktree outside the
  checkout that holds `.mote/` finds no store, or the wrong one.
- **Store identity.** `store_id` (`st-<ULID>`) is in `.mote/FORMAT.json`, and
  every event carries it.
- **Actor.** Mote tries `--actor`, then `MOTE_ACTOR`, then
  `.mote/local/actor`. When more than one session lease is live, it refuses
  writes whose actor came from the local file (`cli.rs:1527-1551`). Claims,
  reservations and reviews record the actor only, not a session.
- **Ops.** Each op is an immutable file. The reducer replays them in filename
  (timestamp) order, and the order decides acceptance. A late op carrying an
  earlier timestamp can therefore change, after the fact, which of two racing
  ops won.
- **Exit codes.** 0 ok, 2 rejected by the reducer, 3 invalid, 4 store or
  duplicate error, 1 internal (`main.rs:43-53`).
- **Output.**
  - `reserve` and `candidate` commands print JSON.
  - `unreserve`, `claim`, `release`, `begin` and `handoff` print none; a
    rejection appears as `rejected: <reason>` on stderr with exit 2.
- **Idempotency keys.** Supported for messages, candidate mutations,
  heartbeats and roles. Not supported for reserve, unreserve, claim, release,
  begin or handoff. A retried reserve creates a new reservation id and is
  rejected as a same-actor duplicate.
- **Events.**
  - `mote events --json [--after OP_ID] [--follow]` emits accepted ops as
    `mote.event.v1`, and the cursor is the op id.
  - A late op that sorts before the cursor is skipped on resume.
  - An event already emitted can later become rejected.
  - Derived events (reservation expiring or expired, presence, stale
    requests) have synthetic ids.
- **Handoff.** `mote handoff` transfers the claim (compare-and-set on the
  claim clock) and, with `--release`, closes the sender's reservations. It
  does not transfer reservations. While the issue stays open, the recipient
  cannot adopt them.
- **Candidates.** A review is per reviewer: approve, block or comment, updated
  by compare-and-set. The proposer cannot review their own candidate.
  Supersede carries no reviews forward. `landability` lists blocking reasons.

## Contract

### 1. Transport

Fray runs the `mote` binary and parses `--json` output; it never opens Mote's
files. Every call passes `--store`, `--actor` and `--json` explicitly, and has
a bounded timeout (10 s for reads, 30 s for mutations). The adapter lives in
the client, never in the daemon. A slow or hung Mote must never hold the
store lock that serializes every agent's requests.

### 2. Which store

A board and a Mote store are bound once, and the binding is checked on every
call:

1. `MOTE_STORE`, if set, is used as given.
2. Otherwise the adapter uses `.mote/` in the main worktree: the parent of
   `git rev-parse --path-format=absolute --git-common-dir` for a non-bare
   repository. That is the same root from which Fray derives its shared
   board, so every linked worktree, including ones outside the checkout, uses
   one store.
3. On first use, the board records that store's `store_id` in its `meta`
   table (`mote_store_id`). A later call against a different `store_id` fails
   with `mote_store_mismatch` and names both ids; nothing falls back silently.
4. No `.mote/` means no adapter. Fray behaves exactly as it does today, and
   says "Mote not adopted" wherever an adapter feature would have acted.

### 3. Which actor

- The Mote actor is the Fray identity name, passed with `--actor`. The
  adapter never relies on `.mote/local/actor`, so Mote's multi-session guard
  cannot silently misattribute a write.
- Fray already refuses a second live session under one name (sessions,
  Phase 1.1). While that holds, name to actor is one-to-one, and it is
  collision prevention, not authentication.
- A mutation with no Fray identity is refused (`mote_actor_unresolved`).
- The owner never acts in Mote through the adapter.

### 4. Reads and mutations fail differently

| Call | Examples | On failure |
|---|---|---|
| Read | `who-has`, `preflight`, `show`, `candidate show`, `events` | Degrade to advisory: continue with Fray's own view and print one warning naming the command and error (`Mote unavailable; lanes are advisory only`). Never block a commit or a message. |
| Mutation, rejected (exit 2) | `reserve`, `unreserve`, `claim`, `handoff`, `candidate review` | Fail with `mote_rejected` and Mote's reason. Record nothing in Fray. |
| Mutation, unconfirmed (exit 1, 3 or 4, timeout, unparseable output) | same | Re-read Mote's state to learn the outcome (see 5). If that re-read also fails, fail with `mote_unconfirmed` and name the read the agent can run. Never report success; never record a substitute. |

### 5. Idempotency without Mote keys

For ops Mote can key (candidate review, messages), the adapter passes a key
derived from Fray's own request key, so a retry is safe end to end.

For ops without keys, the adapter checks before it writes, and checks again
if the outcome is uncertain:

- **reserve:** `who-has` for each path. If a live reservation held by this
  actor on this entity already covers the path, it is reported as present,
  not created again.
- **claim and handoff:** `show` the issue and compare the claim holder with
  the intended holder.
- **unreserve:** `who-has` again. If no reservation remains, the unreserve is
  done.

Each check is a read, so it degrades under the rules in 4.

### 6. Mote to Fray attention

A small `mote_events` table in the board records `(store_id, event_id)` as the
primary key, together with the Fray card it produced. The `events` cursor is
stored in `meta` (`mote_cursor`).

A listener run by the client (`fray mote sync`, and on each `brief`) reads
`mote events --json --after CURSOR`. For each event relevant to an agent, it
writes the attention card, the `mote_events` row and the new cursor in one
Fray transaction:

- a reservation that is expiring or expired;
- a candidate becoming landable, or blocked;
- a review requested from the agent;
- a handoff to the agent.

The consequences:

- **Exactly-once attention.** A duplicate `event_id` is ignored.
- **Mote committed, Fray write failed.** The cursor did not advance, so the
  next sync replays the event and produces exactly one card.
- **Fray crashes mid-sync.** The whole transaction rolls back; the restart
  converges.
- **Skipped and flipped events.** The event feed can skip a late op, and an
  accepted op can later be rejected. Every sync therefore also runs a bounded
  snapshot reconciliation: the agent's claims and reservations from `mote ls`
  and `who-has` are compared with the attention cards it holds. Wrong cards
  are superseded with a note; no card is ever treated as authoritative.
- **Mote ownership in cards.** A Fray card produced from Mote carries the
  `mote:ID` reference and says that Mote holds the state. Resolving the card
  never changes Mote.

### 7. What each dependent child gets

- **Lanes and the git guard (child 4).** With Mote adopted, `lane take` calls
  `mote reserve` for the lane's paths under the card's `mote:` issue, and the
  guard consults `who-has`. A Fray lane becomes a view of the Mote
  reservation plus Fray's queue and notification order. A rejected reserve
  refuses the lane.
- **Review and landing (child 1).** A Fray verdict is recorded as a Mote
  candidate review (approve maps to approve; object or blocked maps to
  block), keyed so a retry is safe. The Fray card is the conversation. Mote's
  self-review refusal and quorum policy apply unchanged.
- **Handoff (child 6).** Mote cannot transfer a reservation, and the
  recipient cannot reserve overlapping paths while the sender holds them. So
  a handoff cannot avoid a gap without a Mote change. The adapter makes the
  gap short and visible:
  1. Post the handoff packet.
  2. `mote handoff --to` transfers the claim.
  3. The sender runs `mote unreserve`.
  4. The recipient's first `accept` runs `mote reserve`.

  A Fray lane is held across steps 3 and 4 and announces the gap. Each step
  is idempotent under 5, and `fray handoff --resume` continues an interrupted
  sequence. Child 6's acceptance criterion "no window in which the paths are
  unreserved" is not achievable on Mote 0.1.0. It needs the Mote capability
  in the next section, and is marked blocked on it.

## Requests to Mote

These are not filed in the Mote repository. Filing there would affect
another project, which the charter reserves for the owner.

1. Transfer reservations on handoff, atomically with the claim.
2. Idempotency keys on reserve, unreserve, claim, release, begin and handoff.
3. JSON output for unreserve, claim, release, begin and handoff.
4. A stable acceptance order. With order-derived acceptance, a late op can
   retroactively overturn a reservation or claim that Fray already reported
   as held. Until this is fixed, Fray re-reads Mote at the points where it
   matters: `fray land` and the pre-push guard.

## Acceptance (from the bead)

- The tests run with and without `.mote/`.
- All worktrees, including one outside the checkout, resolve one store, and
  a mismatched `store_id` is refused.
- A read outage produces a warning; a mutation outage fails loudly and
  records nothing.
- Replaying the same Mote op yields exactly one attention card.
- A failure injected between the Mote commit and the Fray write recovers on
  the next sync.
- When `.mote/` exists, Fray writes no state that Mote owns.

Tests use a stub `mote` on `PATH` (a script that records its argv and returns
canned JSON and exit codes) for failure injection, plus the real binary in a
temporary store for the happy path.
