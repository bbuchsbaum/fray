# Design: no silent stalls

Status: proposal for review (2026-09-30). This addresses four field reports
from the ScalaFIM campaign:

- asks stall silently (bd-01M3SVTMQB80J9W1VQAV1FPF22);
- asks go to a named agent rather than a role (bd-01M3SVTMSE44S9265QMHSH8H5G);
- interactive wake coverage lapses silently (bd-01M3SVTMV6XPX099A2DG826ES3);
- Mote messages and Fray cards are split channels
  (bd-01M3SVTMWTMX3CV35CW219ESVX).

It also covers the "restart needed" note on bd-01M3S3KRC8RBJ3X5H4RCPV4ZCC.

## The failure these share

A request goes somewhere nobody is listening, and nothing says so. In each
report the requester believed the request was in flight:

- The recipient was idle.
- Or it was listening on the other channel (Mote messages versus Fray cards).
- Or its wake had quietly expired (Claude's Monitor caps at 30 minutes).
- Or it had never joined.

The request sat for hours, until a person noticed. A request that is visibly
stuck is recoverable; one that silently looks like progress is not. Fray's
job is the attention around work, so a request with no listener is Fray's
problem to surface.

## Principles

1. **Say it at the moment it can be acted on.** Warn at send time when the
   recipient cannot currently be reached. Tell the requester when a deadline
   passes. Tell the agent, at its next turn, that its wake lapsed.
2. **One listener is enough.** An agent that watches Fray also sees the Mote
   requests addressed to it.
3. **No new authority and no silent re-routing.** Fray suggests a re-route
   and makes it one command; it never moves a request on its own. Routing to
   a role is explicit.
4. **State lives where it already lives.** Deadlines are card fields, role
   holders come from Mote's role leases, and listener leases come from
   Fray's existing tables. No new scheduler: staleness is computed when read.

## Slices

Each slice is reviewed and lands on its own, in this order: the first two
are the largest cause of stalls at the smallest cost.

### S1. Send-time reachability, and Mote messages as attention

**Reachability at send.** `fray send NAME --ask` (and `reply` to an ask
addressed to someone) reports the recipient's reachability from the presence
data Fray already has:

- `live`: an active session or a recent write;
- `armed`: a connected listener or a running `drive` controller;
- `idle for 3h, no listener`.

The ask is still created, because queuing stays the default. When the
recipient is neither live nor armed, the result carries `unreachable: true`
and names the alternatives: a role (S4), the owner (`fray ask-owner`), or
`--pending` for a name that has never joined. `--require-reachable` turns the
warning into a refusal, for scripts that must not queue into a void.

**Mote messages as attention.** `fray mote sync` adds the `message` event
category.

- A `message.sent` whose `msg_kind` is `request` becomes a p1 card to its
  recipient, under the key `mmsg:<msg_id>`, reusing the adapter's
  exactly-once ingest. Other kinds become p2 cards.
- A private Mote message is reported without its body: "private Mote message
  from X; read it with `mote inbox`".
- Responses and resolutions (`message.replied`, `message.resolved`) reach
  the requester.

Then one listener, `fray watch`, a hook or `drive`, carries both channels.
The reverse direction needs no bridge: a Fray card carries a `mote:` ref.

**A name only Mote knows.** `fray send` to a name that has never joined
Fray, but that is an actor in the paired Mote store, says so and suggests
`mote send NAME --kind request`, so the request reaches where that agent
listens.

### S2. Deadlines on asks

- **Setting one.** `fray send NAME --ask --respond-within 30m` (also on
  `reply --kind question` and objections) stores `due:<ms>` as a tag. Adding
  it later with `patch --respond-within` is the author's call.
- **Overdue.** An ask is overdue when it is past due, still open, and has no
  response from its addressee. "Response" means the same as in `fray stats`:
  an annotation by the assignee, the lease holder or the owner.
- **Where it shows.**
  - The requester's `brief` and `inbox` gain an `overdue_outgoing` section,
    so the requester is told, not only the recipient.
  - `fray friction` lists overdue asks first.
  - The recipient's hook context marks the item overdue.
- **Escalation is one command, never automatic.** The overdue notice names
  the options:
  - `fray patch ID --assignee OTHER` to re-route;
  - `fray send @role:R` (S4);
  - `fray ask-owner --card ID`.
- **Without a deadline.** Asks with no deadline get a soft default in
  `friction` only: flagged when unanswered for over 24 hours, and in `brief`
  after 4 hours. Mote's `--request-stale-after` (1 hour) is shown as the
  suggested default.

### S3. Wake coverage that cannot lapse silently

- **Lease expiry.** Listener leases already expire. The store records the
  time a listener's lease lapsed (`listeners.lapsed_ms`).
- **Telling the agent.** At the agent's next turn boundary, in the
  `PostToolUse` or `SessionStart` hook and in `brief`, the context says:
  "your wake lapsed at T; N asks addressed to you arrived since; re-arm with
  <command>".
- **Telling requesters.** They see "no armed listener since T" in S1's
  reachability.
- **One re-arm command.** `fray arm` prints the host-appropriate command to
  arm at the maximum expiry. For the Claude Monitor that is `watch
  --attention --notification --activation native-monitor
  --activation-expires-ms 1800000`, with "coverage until HH:MM". It
  registers nothing itself, because only the host can arm a wake.
- **Standing responder.** A documented pattern for agents that must answer
  while unattended, such as a reviewer:
  - a `fray drive` worker is woken only by asks to it or to its role (S4),
    with a bounded budget and turns;
  - a worked example goes in the skill.

  Interactive sessions cannot be woken from idle; driven ones can, and the
  documentation says so plainly.

### S4. Asks to a role

- **Resolving the role.** `fray send @role:reviewer --ask` resolves the role
  through Mote's role leases (`mote role`), read by the adapter. Among the
  current live holders it picks the one that is reachable, per S1. If several
  are, it picks the least recently asked.
- **Recording the route.** The card records both the role and who it went to
  (`routed:role:reviewer`).
- **No holder.** With no live, reachable holder, the send fails at send time
  (`role_unreachable`), listing the holders and their states. It never queues
  into the void.
- **Fallback chain.** `--fallback owner` or `--fallback NAME` is an explicit
  chain that is tried in order when the role cannot be reached.
- **Without Mote.** A board without Mote can declare roles in Fray itself
  (`fray role take reviewer`, a lease like a lane). This is deferred unless
  needed.

### S5. "Restart needed" is visible to whoever can act

- **Detecting it.** When a client's build differs from the daemon's and the
  client has capabilities the daemon lacks, the daemon learns this from the
  client's handshake. It keeps a `restart_needed` record: the newest client
  build seen, and the capabilities it is missing.
- **Showing it.** `fray agents`, `doctor` and the owner's `fray owner review`
  show "restart needed since T (clients on build X lack: ...)". Stewards see
  it in `brief`.
- **No automatic restart.** Restarting stays a deliberate, announced act.

## What this does not do

- **No automatic re-routing, and no pages outside Fray.** Push notifications
  to the owner's phone and similar may come later, as a host concern.
- **No change to Mote.** Mote messages are read, never written, except when
  the agent runs `mote` itself.

## Acceptance

- **S1:**
  - A send to an idle, unarmed agent reports that the agent is unreachable
    and names the alternatives.
  - `--require-reachable` refuses such a send.
  - A Mote request reaches the recipient's Fray inbox exactly once, and a
    private one carries no body.
  - A send to a name known only to Mote suggests `mote send`.
- **S2:**
  - An overdue ask appears in the requester's `brief`, `inbox` and
    `friction`, and stops appearing once the addressee answers.
  - A reply by a bystander does not clear it.
- **S3:**
  - After a listener lease lapses, the next hook or `brief` for that agent
    says so, with the count of asks since and the re-arm command.
  - A requester's send shows "no armed listener since T".
- **S4:**
  - A send to a role with a live holder reaches that holder.
  - With no holder, the send fails with the holders' states.
  - A fallback chain is followed in order.
  - It works against the real `mote` role leases.
- **S5:**
  - A new client against an old daemon records "restart needed".
  - `agents` and `doctor` show it.
