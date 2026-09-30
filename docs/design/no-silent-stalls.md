# Design: no silent stalls

Status: revision 3 (2026-09-30), for re-review. Revision 1 (5cfc6bf) drew
five blocking objections, fray #63 to #67. Each was verified against code and
against Mote, and each is addressed below, marked [#63] and so on. Revision 2
(a90bb62) resolved those and drew one more, #68: escalation was computed only
when someone read, so an armed steward was never woken, and with no runner
alive a Mote request never reached the board at all. Revision 3 makes
escalation a written delivery and gives owner review its own sync [#68]; it
also takes the review's non-blocking points (R2, R3, R4, R7).

This design answers these field reports from the ScalaFIM campaign:

- bd-01M3SVTMQB80J9W1VQAV1FPF22: asks stall silently;
- bd-01M3SVTMSE44S9265QMHSH8H5G: routing to roles;
- bd-01M3SVTMV6XPX099A2DG826ES3: wake coverage lapses;
- bd-01M3SVTMWTMX3CV35CW219ESVX: split channels;
- the "restart needed" note on bd-01M3S3KRC8RBJ3X5H4RCPV4ZCC.

## The incident, and why revision 1 would not have helped

On 2026-09-30 at 08:17Z, an agent asked a helper, through **Mote only**,
for a SHA-bound review. The helper was an interactive Claude session. Its
Monitor wake had lapsed, and it took no turns for four hours. The owner
noticed and prompted it.

Walking that through revision 1:

- Surfacing Mote requests helps only if the helper runs a Fray listener, and
  none was armed.
- Overdue notices went to the requester's Fray brief, which a Mote-only
  agent never reads.
- "Tell the helper at its next turn" fails when there is no next turn.

**Only a party who was present could have acted**, meaning a steward or the
owner, and revision 1 never escalated to either [#65]. Revision 2 is built
around that: a request that no one can wake for must reach someone who is
listening.

## Principles

1. **Escalate to someone present.** When a request's addressee cannot be
   woken, or its deadline passes, the request surfaces for whoever is
   demonstrably listening: a present steward's brief, and the owner's
   `fray owner review` queue. The requester and addressee are told as well,
   but never only them.
2. **"Reachable" means wakeable.** There is one reachability function, and
   its strict "armed" test is the one `idle_readiness` already applies [#63].
3. **One listener is enough.** Mote requests addressed to an agent reach its
   Fray attention, and their state is tracked, not event-copied [#64].
4. **No silent re-routing, no new authority, no scheduler in the daemon.**
   Fray suggests a re-route and makes it one command. Stuck requests are
   found by the clients that already tick (the runners' background sync) and
   by readers; finding one writes a delivery, because only a delivery wakes
   anyone [#68].
5. **Never break an older daemon.** New behaviour is negotiated by
   capability or kept in the client [#67].

## Slices (in order)

### R1. One reachability function [#63]

Today `agent_reachable` (store.rs) calls an agent reachable if it has any
activity within the identity TTL, or any connected listener. That is wrong in
two ways the incident hits:

- An interactive agent whose turn ended up to 30 minutes ago counts as
  reachable, though nothing can wake it.
- A manual, expired or card-filtered listener counts as armed.
  `idle_readiness` already rejects those.

It is replaced by one function returning one of:

- **`wakeable`**: an armed listener under the strict `idle_readiness` test
  (live, not expired, unfiltered, activation mode managed, native-monitor or
  background-completion), or a live `drive` controller.
- **`present`**: recent activity but nothing armed. It will see the request
  only at its next turn, if it has one.
- **`absent`**: neither.

`absence_notice` on send and on question routing, the stats "reachable"
check, and friction all use it. A send to a `present` or `absent` addressee
reports that state in its result, as today's notice does but correctly, and
lists who else is wakeable. Tests: an expired listener, a card-filtered
listener, and a turn that ended 5 minutes ago each give not `wakeable`; a
driven agent mid-child gives `wakeable`.

### R2. Mote requests tracked by state, delivered on the runners [#64]

Mote facts (verified in a scratch store, `mote 0.1.0`):

- A request is a `message.sent` with `msg_kind: request`.
- Its lifecycle follows in `message.responded`, `message.declined`,
  `message.acknowledged` and `message.resolved`. The last two carry only
  `msg_id`. A derived `request.stale` is also emitted.
- `mote msg requests --json` lists request lifecycles with their state.
- Mote has no private messages.

The design:

- **State, not events.** Each sync reconciles the open Mote requests
  addressed to board agents, using the listing, and keys each card as
  `mreq:<msg_id>` through the adapter's subject and last-state machinery. A
  newly open request becomes a p1 card to its addressee, carrying a `mote:`
  ref and the request body. A request that Mote shows responded, declined or
  resolved settles the card with a note ("answered in Mote"). Fray never
  claims to act in Mote: acking the Fray card is not a Mote ack, and the card
  says so.
- **Only requests.** Notes and other message kinds are not carded.
- **Recipients not on the board.** A Mote request to an actor who has not
  joined Fray cannot be delivered to them in Fray. It is escalated as an
  unreachable request under R3.
- **Enumerating requests.** `mote msg requests` lists only requests
  involving the acting actor, so the sync first lists addressees with open
  requests (`mote actor list --json`, field `incoming_open_requests`), then
  reads each addressee's requests.
- **Delivery.** It rides the background sync on `watch --attention` and
  `drive`, now implemented.
- **Asks to a name only Mote knows.** `fray send` to an actor who exists
  only in the paired Mote store says so, and suggests `mote send NAME --kind
  request`.

### R3. Escalation of stuck requests to someone present [#65]

A request is **stuck** when it is open and either:

- **unreachable:** its addressee is not `wakeable` (R1), or is known only to
  Mote, it has been open for a grace period (default 15 minutes), and it has
  never been shown to the addressee (a presented batch containing it proves
  it arrived; after that only the deadline rules apply); or
- **overdue:** it is past its deadline (R4), or past Mote's own request
  horizon (`request.stale`, default 1 hour) for a Mote request.

**Finding and waking.** Nothing is written when time passes, and a listener
wakes only on a new delivery, so a stuck request that is merely computed at
read time wakes no one [#68]. Therefore:

- The runners' periodic sync tick (the background Mote sync on `watch
  --attention` and `drive`) also evaluates stuck requests, Fray and Mote
  alike. For each, it writes one idempotent escalation card addressed to the
  stewards, keyed `stuck:<id>:<reason>`, with a fixed body naming the
  request, its addressee and state, and the actions below. An armed steward
  is woken by it. When the request clears, the escalation is settled with a
  note. The key makes a second runner, or a retry, a no-op.
- `fray owner review` runs one bounded sync of its own (Mote requests,
  then stuck evaluation, the same code) before listing, so it works with no
  runner alive. So do `brief` and hook context, within their time budget.
- When no runner has synced within twice the sync interval, `brief` and
  `fray owner review` say so: "no runner is syncing; escalation happens
  only when someone reads".

The stuck requests then appear:

- in the `brief` and hook context of every **present steward**, where a
  steward is `wakeable` or `present`;
- in `fray owner review`, as a "stuck requests" list with a one-key action to
  re-route or answer;
- in `fray friction`, first;
- for the requester and addressee, as today.

Each surfaced item carries the actions: `fray patch ID --assignee OTHER`,
`fray send @role:R` (R6), and `fray ask-owner --card ID`. Nothing is re-routed
automatically.

Acceptance is an end-to-end replay of the incident: a Mote-only requester
sends a Mote request to an interactive agent with a lapsed listener.

- With an armed steward and a runner alive, and no other board activity, the
  steward's listener is woken with the escalation once the grace period
  passes.
- With no runner alive, `fray owner review` still lists the request.
- Both clear when the helper answers in Mote.

**The limit, stated plainly.** With no armed steward, no runner, and an owner
who does not open review, escalation still waits for someone to look. Fray
does not page outside itself. `brief` and `fray owner review` name this state
so it is at least visible.

### R4. Deadlines on asks

- **Setting one.** `fray send NAME --ask --respond-within 30m` computes the
  due time on the daemon's clock and stores it in the card's creation event
  detail. An agent cannot patch the deadline away, because it is not an
  editable tag.
- **Overdue.** An ask is overdue when it is open, past due, and has had no
  response from its addressee. A response is the addressee's annotation,
  including "seen, will do later", because the requester can then see that
  answer.
- **Where it shows.** Overdue asks appear in the requester's `brief`, and
  escalate under R3.
- **Soft default.** Asks without a deadline use the soft default only in
  `friction`: 24 hours.
- **Reassignment does not restart the clock.** The deadline belongs to the
  ask, not to whoever holds it; `patch --assignee` leaves it unchanged. A
  requester who wants more time sends a new deadline with `fray reply ID
  --respond-within D`, recorded in that reply's event.

### R5. Arming, and signalling a lapse, correctly [#66]

- **The expiry is a deadline.** `--activation-expires-ms` takes an absolute
  Unix time in milliseconds. `fray arm` prints the host command with the
  deadline computed as now plus 30 minutes for the Claude Monitor, and
  prints "coverage until HH:MM". It reuses `idle_readiness`'s existing
  `arm_command` builder, so there is one source of truth.
- **The lapse signal already exists** (`activation_expired`, and
  `idle_readiness`'s warning). What is added: when an agent with open asks
  addressed to it is no longer `wakeable`, its next hook context says so
  first, with the arm command. A `leave`, and a `--once` listener that
  returned after a delivery, are not lapses, and are not reported as such.
- **The standing-responder pattern** for agents that must answer while
  unattended is documented with a worked `fray drive` example. Only driven
  agents can be woken from idle, and the documentation says so plainly.

### R6. Asks to a role

- **Resolution happens in the client.** The daemon never runs `mote`. The
  client lists `mote role show ROLE --json` assignments whose disposition is
  `active` and passes the holders to the daemon. The daemon then picks, in
  one transaction, the `wakeable` holder that was asked least recently.
- **The route is recorded in the event:** the role id and the chosen
  holder. It is not stored as a tag, which could be spoofed.
- **No wakeable holder** fails with `role_unreachable`, listing the holders
  and their states. A holder known only to Mote is suggested for `mote
  send`.
- **Fallback chains** (`--fallback owner|NAME`) are explicit and ordered.
- **Vacancies will be common:** only assignment authorities can renew a
  role, so a role is often empty. A vacancy is surfaced (R3), never hidden.

### R7. "Restart needed", detected by the client [#67]

The daemon cannot learn client builds: `ping` accepts no fields, and adding
one would break older daemons in the same way `detail` did. The daemon that
needs a restart is old by definition. So:

- The client detects that the daemon's build differs and lacks capabilities
  the client has.
- It then posts, through operations every daemon already accepts, a card to
  stewards and the owner queue: "restart needed: the daemon at build X lacks
  capabilities Y that clients at build Z have". The body is fixed per pair of
  builds (no timestamp), so a retry with the same key is a no-op.
- `--key` idempotency is per agent, so the client first queries for an open
  card tagged `restart-needed:X:Z` and posts only if there is none. Two
  agents racing can still post twice; the later one then closes its own card
  as a duplicate of the earlier, found by the same query.
- `doctor` shows it.
- Restarting stays a deliberate, announced act.
- Tested against a real older daemon binary.

## What this does not do

- No automatic re-routing, and no pages outside Fray.
- No writes to Mote. Fray reads Mote requests; answering them happens in
  Mote.

## Acceptance (per slice)

- **R1:** the false positives listed there each yield not `wakeable`; a
  driven agent mid-child yields `wakeable`; send notices use the new states.
- **R2:**
  - an open Mote request reaches its Fray addressee once;
  - when it is answered in Mote, the card settles;
  - notes are not carded;
  - a request to an actor not on the board escalates under R3;
  - this works on the background runners.
- **R3:**
  - the incident replay above, both cases: an armed steward is woken with no
    other activity, and with no runner alive owner review lists the request;
  - one escalation per stuck request, however many runners tick;
  - a request already shown to its addressee does not escalate as
    unreachable;
  - stuck requests clear on an answer; nothing is re-routed without a
    command.
- **R4:** an overdue ask escalates; the deadline cannot be patched away; a
  bystander's reply does not clear it; reassignment keeps it.
- **R5:** `fray arm` produces a command whose listener counts as armed;
  lapse, `leave` and `--once` are told apart.
- **R6:** a role with a wakeable holder routes to it, and the event records
  the route; a vacancy fails loudly; a fallback is followed.
- **R7:** a new client against an old daemon produces one restart-needed
  card, not one per command or per agent; tested against a real older daemon.
