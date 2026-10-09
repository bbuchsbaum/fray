# Fray

Local collaboration for coding agents. Ask a peer, discuss evidence, surface an
objection, hand work over, and return to a project with useful current context.

Fray connects independent agent processes through one Rust binary, a Unix socket,
and SQLite. It works alongside Mote: **Mote tracks the work; Fray carries the
conversation.** One or two agents can act as stewards, maintaining shared context
and routing questions. Workers can collaborate directly without them.

The core has five direct dependencies and makes no model calls. Codex, Claude,
and DeepSeek-backed agents use the same CLI/protocol; the host determines how
updates reach its model. This version runs on one workstation. The
[architecture](docs/ARCHITECTURE.md#9-future-multiple-workstations) leaves room for
an authenticated network transport around the same authoritative store.

Status: locally tested development version. See the [source review](docs/REVIEW.md)
and [validation evidence](docs/VALIDATION.md) for results and qualification limits.

## Build

Requires stable Rust, a C toolchain for bundled SQLite, and Linux or macOS.

```sh
cargo build --locked --release
cargo install --locked --path .
```

The binary is also available directly at `target/release/fray`. Nothing needs to
be registered with an external service.

### Upgrading an installed binary

`cargo install` replaces the binary safely. To install a binary you have
already built, use `scripts/install.sh [SOURCE] [DEST]`, which by default
installs `target/release/fray` to `~/.cargo/bin/fray`. Never `cp` over an
installed `fray` while it is running, and a daemon usually is. Copying
rewrites the running file in place, and on macOS every later launch of that
path is then killed: exit status 137, no output, even for `--version`. The
script:

- writes a new file beside the target and renames it into place;
- keeps the previous binary as `.previous` for rollback;
- checks that the installed path runs, and explains exit 137 if it does not.

To recover from an in-place copy, run the script again. To roll back a
bad install, run `scripts/install.sh ~/.cargo/bin/fray.previous`; that copy
is only known-good if the install before it was. A rollback swaps the two,
so `fray.previous` then holds the bad build: do not roll back twice.

Installing does not restart a running daemon. Announce the restart on the
board, check that no one has a live wait, then run `fray stop` and
`fray start`.

## Start a conversation

Run from the project the agents are working on:

```sh
fray start
fray --as manager join --role steward
fray --as codex join --topics parser
fray --as claude join --role reviewer --topics tests
fray --as deepseek join --topics evidence

fray --as manager send codex 'Is the parser ready for review?' \
  --ask --ref mote:parser-42 -p 1
fray --as codex inbox
```

Use the returned card ID for the conversation and the exact `through_seq` from
each inbox receipt. `ID` and `THROUGH_SEQ` below are placeholders:

```sh
fray --as codex thread ID
fray --as codex reply ID 'Implementation ready; please review the empty-input case.'
fray --as codex ack ID --through THROUGH_SEQ

fray --as claude reply ID 'The empty-input case lacks a regression test.' --kind objection
fray --as manager thread ID
fray query --ref mote:parser-42
```

`send` opens a note; `send --ask` opens an actionable question. `reply` appends to
the existing conversation. Repeatable `reply --ref mote:ISSUE` adds searchable
references without replacing existing ones, and records them in reply history.
Question and objection replies also attach those references to the linked question.
This requires a daemon advertising `reply_refs`; coordinate an upgrade/restart
with its owner if the client reports that capability missing. An objection or question reply also creates a linked,
independently resolvable question so it cannot disappear in a long thread.
Closing the original conversation does not close its objections.

Only the objector or owner can close a linked objection. Closure notifies the
objector; an owner override also alerts the owner and objectors through fresh
addressed notices, even when the source conversation was muted. Open objection
IDs and override reasons appear in bounded attention views.

Addressed sends use topic `@RECIPIENT`. They reach the recipient, stewards, and
participants; ordinary `*` subscriptions exclude addressed topics. All content
is public and searchable. A peer who contributes joins the conversation and
receives subsequent updates even outside its topic subscriptions. `leave` stops
new deliveries on every route; existing unread receipts remain. Explicit rejoin
seeds current live heads and catches up previously known conversations, including
their terminal outcomes. Direct requests created and closed during the absence
are also delivered on rejoin.
Receipt alone is not participation. `follow ID` opts in; `unfollow ID` removes
explicit following but does not override direct routing or topic subscriptions.
Changing `join --topics` preserves old receipts and reports those outside scope.
`mute ID` suppresses that exact card in attention and future deliveries without
acknowledging anything; `unmute ID` restores eligible missed peer updates. It
does not subscribe an unrelated agent. Muting survives rejoin. Open questions
and objections assigned to you cannot be muted, and old mutes do not hide later
assigned requests. A linked objection stays visible when its parent is muted.

`ack` means the agent considered that version. It does not imply agreement or
completion. A stale acknowledgment cannot consume a newer update. `thread`
includes current state, ordered history, and exposure/acknowledgment receipts.
Long histories paginate with `--after` and `--limit`.
Use `fray thread ID --bodies` for ordered sequence, author, kind and complete
message text, preserving line breaks. `--json` still returns the full structured
history. The human view reports the next `--after` when a page is incomplete.

Use `fray --json thread ID --compact` for one current head plus ordered events
without repeating a whole head per message. `thread ID --unread` starts at your
durable acknowledgment and includes open linked questions and objections. Both
views paginate with `--after` and `--limit`; reading a next page requires no ACK.
The readable thread headers include `rREV` for `patch --expect REV`.

`send` and `reply` accept up to 8,000 UTF-8 bytes, inline, from stdin (`-`), or
from a file:

```sh
fray --as manager send codex --body-file contract.md --ask --ref mote:parser-42
fray --as codex reply ID --body-file findings.md --kind evidence
fray --as manager inbox --addressed-to-me --unresolved
```

Long initial messages have a bounded card summary; their exact full text stays
in the creation event and historical search. Card summaries remain limited to
2,000 bytes. These are stored message bodies, not filesystem attachments.
`--addressed-to-me` selects pending conversations whose current assignee is you;
`--unresolved` excludes resolved, superseded and withdrawn heads. Neither filter
acknowledges hidden receipts. Use `--selection involved` to include replies to
your outgoing questions and conversations you joined. Long sends and inbox
filters require the daemon capabilities `long_messages` and `inbox_filters`;
the client fails before sending an unsupported operation.

Each inbox item also includes a store/agent-qualified `receipt` object. Save the
packet, handle its messages, then pass the handled receipt array to
`fray ack --receipts -` on stdin. The batch is atomic. Never use `card.last_seq`
as a receipt: your own reply can advance the head without delivering to you.

On capable daemons, `inbox`, `wait`, `thread --unread`, and attention streams also
return an immutable batch token. Fetch it with `fray batch TOKEN`, then acknowledge
only the cards considered with `fray ack --batch TOKEN --ids 3,4` (omit `--ids`
only after considering the whole batch). Newer replies stay pending. Tokens survive
daemon restart, belong to one store and agent, and expire after 24 hours or when
superseded by 32 newer batches for that agent. `ack --last` selects this session's
latest explicit inbox/thread batch; background waits and watches are excluded.

Overlapping receipts need only one acknowledgement. After reading and handling a
thread, acknowledge its batch; the same card/version in an earlier watch batch is
then handled too. Other cards and newer versions still need consideration. When
replying, combine the reply and acknowledgement:

```sh
fray thread ID --unread
fray reply ID 'Verified the new evidence.' --ack-batch BATCH
```

`--ack-batch` acknowledges **only the reply's card** at the version recorded in
that batch. Reply and acknowledgement commit together or neither does. This
requires the daemon capability `reply_ack_batch`. Ordinary replies never ack.
Replies warn about peer updates beyond acknowledged history or this session's
explicit inbox/thread receipts; background notifications do not suppress the
warning. This is a conservative receipt check, not proof of what a model read:
plain `thread --bodies` reads have no such receipt, unbound sessions have no
shared read marker, and previews can be truncated.

`watch --attention --notification` includes the latest peer message's kind,
first nonempty line and sequence in a bounded `preview` alongside the card
title. Author is included when space permits. Text can be shortened and trailing
card previews omitted to fit; the batch/receipt identifiers remain exact.

## Peer discovery and review evidence

Newly observed peers appear at bound-session CLI boundaries and supported host
SessionStart/PostToolUse hooks, even without a high-priority message. Notices name
the exact peer, role, recent activity and listener state. `fray peers` displays
the next bounded page; `fray agents` remains the full current roster. A notice is
suppressed for that session only after successful output. Rejoin or session
replacement makes the peer visible again. Peer-only news never blocks Stop and
does not wake an idle host by itself. Unbound callers can inspect peers but do
not consume another session's notices.

For uncommitted reviews, `fray snapshot create --paths src tests` captures working
bytes, deletions and nonignored untracked files in a shared, verifiable bundle.
`fray review request`, `review subject`, and `review verdict` keep a fixed baseline,
current candidate and verdict revision together. Previous verdicts become visibly
stale when the candidate moves. See [evidence bundles and versioned reviews](docs/EVIDENCE.md)
for commands, compatibility and limits.

To address a live role holder, use `fray send @role:reviewer 'Please review this'`.
On a paired Mote board, routing requires an active Mote role assignment and a
present or wakeable Fray recipient. Wakeable peers are preferred, with name
ordering as the tie-breaker. Without Mote, Fray uses registered roles. A missing
live holder fails before queuing a message; ACK and routing confer no ownership.

`fray peek` reads a bounded peer/lane/conversation snapshot without joining,
acknowledging, or refreshing presence. For an owner view, `fray board --output
board.html` writes a self-contained HTML snapshot. `fray export --markdown
--since 7d archive/` selects conversations touched in the last week and exports
their complete histories with stable IDs. Existing differing files and output
symlinks are refused. Both commands work while the daemon is offline.

`fray backup state-backup.sqlite` makes and validates an online SQLite backup.
Restore it with `fray --home /path/to/fresh-home restore state-backup.sqlite`;
the destination home must not exist. Restore preserves the original store
identity and receipts, so use it as recovery for that board. Keep recovered and
original copies from running as competing instances of the same board.

History compaction is opt-in and requires an offline board. Preview with
`fray prune --older-than 30d --dry-run`; see [retention](docs/RETENTION.md) before
executing it. This sweep never prunes a shared board.

Explicit Mote candidate review, fenced local landing, dispatch and recoverable
carrier handoff require the [qualified Mote authority capabilities](docs/MOTE_CAPABILITY_GAPS.md).
Standalone Fray review evidence remains advisory. See the
[adapter contract](docs/design/mote-adapter.md) for commands and recovery.

## Shared context and managers


```sh
fray --as manager post 'Parser contract' --kind decision --topic '*' --pin \
  --summary 'Preserve API v2. Track implementation and acceptance in Mote parser-42.'
fray --as codex brief
fray --as manager query --kind question --sort oldest
fray --as manager agents
```

A steward receives project-wide changes but has no extra permissions. Use a
second steward for a distinct responsibility, such as review and acceptance.
Keep decisions and summaries current; avoid progress chatter and acknowledgment
loops. Fray's standalone task/claim commands remain available, but when using
Mote, keep tickets, dependencies, reservations, and task completion there.
Use `mote begin ISSUE --paths FILE...` to combine ownership and reservations;
Fray status/messages can point to that issue without duplicating file claims.
Read-only review does not require a writer lane.

If a pending send used a mistaken name, withdraw or reroute its cards. A name
that never joined disappears from the default roster once it has no open mail.
`fray agents --all` includes those historical recipients; no messages or agent
records are deleted, and later joining still works.

### Joining a team

The owner describes the team once in a pinned owner card titled `Team`
(roles wanted, seams, landing rule, who needs the owner); `docs/TEAM.md`
has a template. In a new terminal, "join the fray" (or "join the fray as a
reviewer", "as a coder on bd-123", "as the steward", "as a monitor") is then
enough: the `fray` skill's "Joining the team" section turns it into a role.
`fray team` prints what a joining agent needs: the Team card, each member's
role, host (from its session) and reachability, Mote candidates waiting on
review, ready beads nobody has claimed, what is stuck, and the gaps.

### Lanes, status and preflight

Where no tracker owns paths, lanes say who is working where. They are
advisory, never locks.

```sh
fray preflight src/store.rs            # declared lanes + real edits in other worktrees
fray lane take src/store.rs tests/ --purpose 'Escalation' --for CARD
fray lane list
fray lane release LANE [--to AGENT]    # release, or hand over
fray status 'reviewing #58; free after'
```

Taking a lane someone else holds is refused; `--queue` waits in order and
tells you when it frees. With no paths, `preflight` checks your changed and
untracked files; `--staged` checks what you are about to commit. A lane whose
holder has been inactive for 30 minutes (or, while it only waits, 4 hours)
shows as stale and may be released by anyone. `status` sets one line per agent, shown in `fray agents`. The skill
gives the full rules.

### The owner

The project owner has a channel of their own. `owner` is a reserved identity:
agents cannot join as it, and its commands refuse to run without an
interactive terminal, so an agent's ordinary tool calls cannot use them. This
prevents accidents and injected instructions; it is not authentication
(`docs/design/owner-authority.md`).

```sh
fray owner decide 'Charter' --summary 'Agents may land reviewed work.'  # pinned
fray owner queue           # open requests waiting on the owner
fray owner review          # stuck requests first, then approve/decline/reply

fray --as codex ask-owner 'May I restart the shared daemon?' --card 42
```

Owner cards carry `authority: "owner (unsigned)"`. Peer text never carries
that authority. `ask-owner` queues a request for the owner and routes the
answer back; the agent keeps working meanwhile.

Three names are reserved and never join: `owner`; `mote`, which authors
attention derived from Mote; and `escalation`, which authors escalations of
stuck requests. Lookalikes of `owner` (`0wner`, `o-w-n-e-r`, `owner1`) are
refused too.

### Working alongside Mote

Where a project uses Mote for tickets, claims and reservations, Mote stays authoritative for them and
Fray carries the attention around them (`docs/design/mote-adapter.md`).

```sh
fray mote status     # which store, whether Mote is reachable, the binding
fray mote sync       # new Mote events into attention, exactly once each
```

Agents rarely need to run `sync` by hand. A long-running `fray watch
--attention` (not `--once`) or `fray drive` syncs Mote in the background about
once a minute for the whole board: runners pace themselves on the last sync
any of them ran, so several runners do not multiply the load. It is quiet on
success, and reports a failure at most once an hour.
`FRAY_MOTE_SYNC_INTERVAL_MS` changes the interval (100 ms to one hour).
`FRAY_MOTE_SYNC=off` stops only the Mote sync; the runner still escalates
stuck requests (see below).

`status` pairs the board with a store only through the rules that chose the
board:

- a sibling `.mote/` of an ancestor `.fray/`;
- `.mote/` in the main worktree for the repository's own board;
- `MOTE_STORE` for anything else, including bare repositories, submodules and
  boards named with an explicit `--home`.

On first use it binds the board to that store's id. From then on a different
store at the same path is refused, never followed. Fray runs `mote` itself,
passing explicit `--store` and `--json`, and `--actor` except on unfiltered events. It never runs
`mote` from the daemon. If Mote is missing, unsupported or unreachable, that
is reported and reads degrade to advisory.

`sync` reads Mote's claim and reservation events after a stored cursor and
posts attention to the agent they concern:

- a reservation expiring or expired, to its holder;
- a claim handed to someone, to the new holder;
- a claim someone else moved away from you, whether by a third-party
  handoff or by taking over after it expired, to you.
- a review requested from you as a named reviewer on a pending candidate;
- a candidate you proposed or authorize becoming landable or blocked, or
  its blocking reasons changing, including returning to an earlier state;
- a candidate's landing, supersession or abandonment, to everyone involved.

Each such event reaches each recipient exactly once, even across interrupted
or concurrent syncs, because the cursor only moves forward, under
compare-and-set. The cards are authored by the reserved `mote` identity, so
they reach the agent that ran the sync too. The first sync starts at the
latest event and never replays history. A Mote actor that has not joined the
board is reported, not notified. Enabled authority stores use admission order,
with raw op-id anchors, revision CAS and separate reservation projections;
timeouts leave that exact cursor unchanged. Legacy stores retain their historical
three-timeout tail reseed. `FRAY_MOTE_READ_TIMEOUT_MS` overrides the
10 s read timeout. Every sync also compares claims with Mote's live board, which is
the truth when it is read. A change the event feed missed, through a late op,
a reseed or a release, is reported by what changed ("you now hold E", "E is
now held by bob"), never by guessing who did it. A release is recorded
quietly.

Strict workflows require upgraded Mote writers sharing the store and enabled
authority (format version 1, or 2 once session-bound claims exist); a
version string is insufficient. Reads never enable authority.
No installed binary or existing store was migrated by the local sweep.

```sh
fray --key review-1 review candidate CANDIDATE --to reader --title 'Review' 'Check behavior'
fray --key verdict-1 review candidate-verdict CARD approve --at git:OID --expect 1 'Verified'
fray land CANDIDATE --target main --check
fray --key landing-1 land CANDIDATE --target main --before OLD_OID
fray --key offer-1 send --to anyone-free 'Bounded work' --mote ISSUE --tag rust
fray --key accept-1 accept CARD --expect 1
fray --key handoff-1 handoff CARD --to peer --state 'Half done' --next 'Finish' --carrier CARRIER=RV
fray --key adopt-1 accept HANDOFF_CARD
fray operation show KEY
fray operation resume KEY
```

Landing is a nonempty local fast-forward through Mote's fence, with no push.
Superseded candidates need new reviews. Local integration-evidence mismatch
notifies the author/evidence producer without re-waking reviewers. Offers and
ACKs do not grant ownership: explicit accept requires Mote readback. A disappeared
peer produces a notice immediately; requeue waits for observed release/expiry.
Carrier close/adopt never unreserves paths, but continuity requires live TTL and
successful readbacks. Competing adoption or expiry reports loss to both parties.
Keys preserve exact requests and receipts for recovery; old retries do not renew
claims, reservations or approvals.

Mote requests (`mote msg send --to NAME --kind request TEXT`) are tracked by
state, not by event. Each sync lists the open requests and turns each one
addressed to a board agent into a p1 ask for that agent, once. The card is
authored by `mote`, tagged `mote:MSG_ID`, and says how to answer:

```sh
mote msg reply MSG_ID 'Reviewed at 1a2b3c: approve'   # or --kind decline
```

When Mote shows the request responded, declined or resolved, the next sync
resolves the card ("responded in Mote"). Acking or closing the Fray card is
not a Mote answer; Fray never writes to Mote. Notes and other message kinds
are not carded. A request to a Mote actor who has not joined the board cannot
be delivered here; it is recorded and escalated as stuck (below). Each sync
reads at most 50 addressees and stops after about 20 seconds, board agents
first; the rest wait for the next sync. Request tracking needs the daemon
capability `mote_requests`; against an older daemon, sync skips it with a
note.

`fray send NAME` to a name that has not joined fails as before. If the paired
Mote store knows that actor, the error says so and gives the Mote command to
ask there instead.

### A guard at commit and push

```sh
fray guard install        # once per repository; covers every worktree
```

This installs `pre-commit` and `pre-push` hooks. When you commit or push
paths that another agent holds, the hook names the holder and how to
coordinate:

- where Mote is adopted, another actor's Mote reservation (active or
  orphaned) counts as held;
- otherwise, another agent's lane counts as held.

The guard only warns. `FRAY_GUARD=block` makes it refuse, and `--no-verify`
remains the escape hatch. It never blocks because Fray or Mote cannot be
reached; it says it could not check. Existing hooks are kept as
`<name>.fray-prior` and run first, with the same arguments and stdin; if a
prior hook fails, the commit or push fails.

### Is collaboration working?

```sh
fray stats                  # all history; or --since 90m / 24h / 7d
fray friction               # worst current offenders, oldest first
fray friction 'the preview truncated my evidence; had to refetch'
```

`stats` replays the event log read-only. It reports:

- time to first response and to resolution, for asks and for objections;
- resolutions over an open objection;
- reassignments, as possible misroutes (a signal, not proof);
- publish-to-first-shown exposure;
- unacked attention per agent;
- lane activity.

Anything the store does not record, such as host wake latency, is listed as
not measured rather than reported as zero.

`friction` with no text lists unanswered asks, open objections, requests
addressed to agents nothing can wake, stale lanes, and recent friction notes.
An ask is marked overdue past its own deadline; an ask without one is marked
overdue after a soft 24-hour default, which applies only here.
With text, it posts a low-priority note on topic `friction`, so friction in
Fray itself is recorded as work instead of lost in chat.

## Put updates into agent context

Give each terminal a unique `--as` name or `FRAY_AGENT`. Install the portable
[skill](skills/fray/SKILL.md) and the appropriate
[host integration](integrations/README.md) in the working project.

```sh
# Run in the project your agents will work on.
fray skill --install both

# Interactive: supplies the identity/environment. Claude hooks supply context.
fray --as claude enter --topics tests -- claude
fray --as codex enter --topics parser -- codex

# Noninteractive: waits for selected attention; empty startup costs no model turn.
fray --as codex drive --max-turns 12 --idle-timeout 300 -- codex exec -
fray --as claude drive --max-turns 12 --idle-timeout 300 -- claude -p
```

The installer writes the same skill to `.agents/skills/fray/SKILL.md` for Codex
and `.claude/skills/fray/SKILL.md` for Claude. Use `--install codex` or
`--install claude` for one host, or `fray skill` to inspect the embedded text.
Existing customized files require a manual merge. Installing the skill does not
modify hooks, project instructions, or host permissions.

Two optional skills capture the collaboration patterns that helped in the pilots:
`fray-seam` agrees on canonical types/APIs and a fixture before separate owners
implement against it; `fray-review` requests an independent check with an explicit
verdict tied to an exact SHA. They favor batched questions and consumer tests across
the ownership boundary. They do not launch peers or grant landing authority.

```sh
fray skill --list
fray skill fray-seam                       # inspect before installing
fray skill fray-seam --install both
fray skill fray-review --install codex
fray skill all --install both              # all three bundled skills
```

Invoke `$fray-seam` or `$fray-review` in the receiving host after installation.
The default `fray skill --install both` still installs only the core skill.
All selected destinations are checked for conflicts before any file is written.

For a DeepSeek-backed host, use its own command that accepts a complete prompt on
stdin, can invoke project tools, and exits when the turn finishes. Fray does not
assume there is a universal `deepseek` command or manage model credentials.

The runner invokes an argument vector directly. Routine prompts contain only a
receipt packet, bounded to 4,000 bytes by default (`--budget`). `--bootstrap`
explicitly requests an initial project briefing, even if there is no attention.
Default `--selection involved` covers direct conversations, outgoing-request
replies, contributions/follows, and explicitly named topics. Wildcard discovery
and steward-wide traffic require `--selection all`; unselected receipts stay durable.
`wait` defaults to `involved`; `inbox` remains `all` by default. Both honor
`FRAY_SELECTION`, which the runner exports.

An invocation with no acknowledged presented receipt stops the runner, regardless
of hidden backlog. `--child-timeout` defaults to 900 seconds; `--max-turns` bounds
invocations. These are not provider spending limits. `fray agents` distinguishes
registration from controller state; the runner heartbeats while busy and idle.
One live runner owns each identity. Stderr emits JSON turn metrics (prompt bytes,
receipt IDs/versions, elapsed time, exit reason); provider usage is unknown/null.
Host authentication, permissions, and approval policies apply.

Interactive Codex gets onboarding and explicit boundary checks; `enter` does not
inject mid-turn updates. Claude hooks supply context at session/tool boundaries.
Codex (0.156 or later, `hooks` feature) runs the same hook with `--host codex`.
Add to `.codex/hooks.json` in the project (or `~/.codex/hooks.json`), then trust
it once with `/hooks` in Codex:

```json
{"hooks":{
 "SessionStart":[{"hooks":[{"type":"command","command":"fray hook --host codex","timeout":5}]}],
 "PostToolUse":[{"hooks":[{"type":"command","command":"fray hook --host codex","timeout":5}]}],
 "Stop":[{"hooks":[{"type":"command","command":"fray hook --host codex","timeout":5}]}]}}
```

With it, Codex hears addressed items after each tool call and cannot end a turn
while an urgent direct request is pending, exactly as Claude does. Set
`FRAY_AGENT` for the Codex process; otherwise the identity is `codex-SESSION`.
Other Codex hook events are accepted and ignored.
An idle interactive terminal is not automatically awakened by a socket event:
hooks run only at turn boundaries, so a Codex or Claude session that has
already stopped hears new mail at its next turn.
Actual model-host acceptance remains to be tested; see
[integration limits](integrations/README.md).

## Session binding and idle readiness

The client sends a host session only when the connected daemon advertises
`sessions`. Precedence is `--session` / `FRAY_SESSION`, then
`claude:CLAUDE_CODE_SESSION_ID` (the hook also accepts its `session_id`), then
`codex:CODEX_THREAD_ID`, otherwise an unbound legacy caller. Other hosts can
supply a stable `FRAY_SESSION`. `enter` and `drive` pass one consistent binding
to their children. A second live session using the same agent name is refused;
`join --takeover` is an explicit, visible override for a departed owner.
Claude Code's `/clear` starts a new session in the same window: the
SessionStart hook continues the identity automatically and records it as
"continued after /clear", so clearing does not lock you out.
When one host runs inside the other (Codex started from a Claude session, or
the reverse) both host variables are set; the client binds the nearer host in
the process tree. Waiting (`wait`, or a connected `watch`) keeps an agent's
lanes for at most 4 hours after its last real activity. Session binding
prevents accidental identity collisions; it is not authentication.

Every agent is `wakeable`, `present` or `absent` (`fray agents` shows which;
docs/design/no-silent-stalls.md, R1). Wakeable means something armed will
bring it back for a card assigned to it: a live `fray drive`, an armed
listener (declared host activation, unexpired, unfiltered), or an unfiltered
`fray wait` in progress. Present means recently active with nothing armed; it
sees new mail only at its next turn, if it has one. Absent is neither. A
connected socket, a heartbeat or a hook is not a wake.

A send to anyone not wakeable says so, and names who else is wakeable or
present:

```text
Note: helper is present (last active 0 min ago) but has nothing armed to wake
it; it will see this at its next turn, if it has one (present, nothing armed:
steward).
```

A question or objection reply goes to the conversation partner, preferring a
wakeable party, then a present one. When that passes over the first party in
line, the result says so ("bob (present, nothing armed) was passed over for
carol, who can be woken") and gives the `fray patch` command to reroute.

`join` and `brief` report `idle_readiness`. Open outgoing questions without an
armed wake mechanism produce a warning and an arm command. Manual, boundary-only,
expired or narrowly filtered listeners do not establish coverage of future
replies. A declared host adapter is still no guarantee of model responsiveness.
Claude's PostToolUse hook surfaces selected urgent direct requests. Stop blocks
once for pending urgent direct requests, for outgoing questions with no armed
wake, and for open asks addressed to you when nothing can wake you. The
continuation guard (`stop_hook_active`) prevents a hook loop. Hooks never
rejoin an explicitly left identity at a tool boundary and honor
`FRAY_SELECTION`.

An agent that is not wakeable and has open asks addressed to it, without a
reply from it, is told
first: a `FIRST:` line at the top of `brief` and of any hook context the hook
emits, and the Stop hook blocks once on it even with nothing new. `idle_readiness.lapse.kind` gives the reason:

- `unarmed`: nothing was armed;
- `lapsed`: the declared activation's expiry has passed;
- `rearm`: a `--once` listener returned (after a delivery, a timeout or the
  end of its session) and must be started again.

Leaving the board is none of these, and an agent that has left is not told.
Only asks you have not yet replied to count: once you annotate an ask, it no
longer keeps you on notice, though it stays open until its asker resolves it.
The outgoing-asks warning gives `fray --as NAME arm` as its arm command. `fray arm` prints the command to arm, with
an absolute expiry, and when coverage ends:

```sh
fray --as helper arm                      # native monitor, 30 minutes
# stdout: fray --as helper watch --attention --notification --selection involved \
#   --reconnect --activation native-monitor --activation-expires-ms 1790858427937
# stderr: coverage until 12:40Z (in 30 min). Start this through your host's
#   monitor with the same lifetime, and rearm before then.
fray --as helper arm --host background-completion --minutes 10
# stdout: the same with --once --activation background-completion instead of
#   --reconnect --activation native-monitor; rearm after each delivery as well
```

`--minutes` (1 to 1440, default 30) should match the host mechanism's own
lifetime; a Claude Code Monitor watch currently lasts at most 30 minutes.
Run the printed command through the host's monitor and rearm before it ends.
`--json` returns the command, `expires_ms` and `coverage_until_utc`. `fray
arm` only prints; it arms nothing by itself.

### A standing responder

An interactive session can be woken only while its host mechanism is armed,
and someone must rearm that mechanism when it ends (a Claude Code Monitor
lasts at most 30 minutes). An agent that must answer for hours while nobody
attends its terminal has to be driven: `fray drive` starts a fresh, bounded
model turn for each selected request. For example, a reviewer that answers
review requests on its own:

```sh
fray --as reviewer drive --idle-timeout 86400 --max-turns 200 -- \
  claude -p --allowedTools 'Bash(fray:*)'
```

Each addressed request starts one bounded model turn with the packet as
input, and `fray agents` shows the reviewer as wakeable while the drive runs.
A drive is bounded on purpose: it stops after a day idle (the longest
`--idle-timeout`), after `--max-turns` turns, or when a child fails or makes
no progress. It then reports why. Read the reason before starting it again;
a drive that has stopped wakes no one, and `fray agents` shows it as stopped
or failed.

## Asks that cannot stall silently

These features answer one failure: a request whose addressee has gone idle
with nothing armed, which nobody notices for hours
(`docs/design/no-silent-stalls.md`).

### Deadlines

```sh
fray send helper 'Review 1a2b3c before the release?' --ask --respond-within 2h
fray reply ID 'Need it by tonight after all' --respond-within 30m
```

`--respond-within` takes minutes, hours or days, from `1m` to `30d`, and needs
`--ask`. The due time is computed on the daemon's clock and stored in the
creating event, not in a tag, so no patch can remove it. Only the asker can
move it, with `reply --respond-within`, which sets a new deadline from now.
An ask is overdue when it is open, past due, and its addressee has not
annotated it since the deadline was set. A bystander's reply does not count;
when the ask has no addressee besides the asker, anyone else's reply does.
Reassigning the ask keeps the deadline, and then only the new addressee's
answer counts. Overdue asks appear at the top of the asker's `brief` and in
`fray friction`, and they escalate as below.

### Stuck requests reach someone present

A request is stuck when it is open and either:

- **unreachable:** its addressee is not wakeable, has never been shown it
  and has not replied to it, and it is older than the grace period (15 minutes; the daemon reads
  `FRAY_STUCK_GRACE_MS`, 0 to one day). Being shown it by `inbox`, `wait`,
  `thread --unread`, a hook, an attention packet or a drive packet proves it
  arrived, and after that only the deadline applies. Reading `brief` does not
  count as being shown. A Mote request to an actor who never joined the board
  counts after the same grace period;
- **overdue:** it is past its deadline or, for a Mote request, more than an
  hour after it was sent (Mote's default request horizon).

For Mote requests, both the grace period and the horizon count from Mote's
`sent_ts`, not from when Fray first saw the request.

Every persistent `fray watch --attention` and every `fray drive` ticks about
once a minute, whether or not Mote is paired. Each tick escalates every stuck
request to every steward who is wakeable or present, except the request's own
asker and addressee. Each steward gets their own p1 card, authored by
`escalation` and assigned to them, so a steward's `--selection involved`
listener is woken. It names the request, its addressee's state and the
actions:

```text
Stuck (unreachable): Review X please
asker's request to helper (card:1:helper, 16 min old; helper is present) is addressed
to someone nothing can wake, who has not seen it. Nothing re-routes it
automatically. Re-route it with `fray patch 1 --expect 1 --assignee NAME`,
answer it yourself, or queue it for the owner with `fray ask-owner --card 1 ...`.
```

The daemon keeps at most one open escalation per request, addressee, reason
and steward, so a card is created once however many runners tick, under
whatever identities. Each tick re-checks every open escalation's request
directly. It resolves the escalation card only when the request is found
clear: answered, closed, shown to its addressee, re-routed to someone
wakeable, or given a later deadline. Otherwise:

- re-routing to another addressee who cannot be woken is a new stuck request,
  and escalates again;
- a steward who closes their escalation card while the request is still stuck
  gets a "Still stuck" card one hour after the escalation they closed;
- a request that clears and later becomes stuck again escalates again.

A tick creates at most 20 new escalation cards; the rest are deferred to later
ticks. Nothing is re-routed automatically. A runner against a daemon without
`escalations`, or running as `owner` or with no identity, does not escalate.

```sh
fray stuck              # read only: what is stuck now, and whether a runner ticks
fray owner review       # the same list first, then the owner queue
```

`fray stuck` also reads Mote directly, read only, for open requests older than
the grace period whose addressee is not wakeable, so it works with no runner
alive. It skips every request the board already tracks, so nothing is listed
twice, and it uses `FRAY_STUCK_GRACE_MS` from the caller's environment. With
nothing stuck it prints `Nothing is stuck.` A board is "not ticking" when no
runner has ticked for more than twice the runners' interval, and never less
than five minutes; a steward's `brief` then says so if stuck requests exist,
and `fray stuck` adds the same line. With no runner, no present steward and no
one reading, escalation waits for someone to look; Fray pages nothing outside
itself. That state is named, not hidden.

## Persistence and operation

The default home is an ancestor `.fray/`, otherwise `fray/` inside Git's common
directory, otherwise `.fray/` in the current directory. Git worktrees therefore
share state. Separate checkouts can share an absolute `FRAY_HOME` on local disk.
Before starting in a sibling checkout, `fray find ..` discovers existing homes
in that workspace, its ancestors, and immediate non-hidden child repositories,
including Git common directories. It reports the selected home and which homes
answer a diagnostic ping. It does not create, start, stop, or select a board.
Choose one with `--home PATH` or export the same `FRAY_HOME` in every worker.
An existing workspace-level `.fray/` is already inherited by both sibling repos;
discovery does not merge boards or find arbitrary custom homes elsewhere on disk.
Keep its path short enough for a Unix socket. Do not share SQLite through a
network or synchronized filesystem.

The CLI checks protocol compatibility on each connection, including watch
reconnects. Compatible package versions may differ (0.2.1 works with a 0.2.0
daemon; both use protocol 2). An incompatible daemon is rejected before sending
operational requests, with both versions and a home-specific recovery instruction.
`start` does not replace a live daemon. `ping` and explicit `stop` remain usable
for diagnosis/recovery; coordinate any restart with other agents first. No fields
are silently removed to accommodate an older protocol.

Check `fray --json ping` capabilities when a deployed daemon rejects a feature.
Installing a new CLI does not replace an already-running daemon, and the package
version alone is not a capability check. `inbox_filters` enables addressed and
unresolved filters; `long_messages` enables `send`/`reply` bodies up to 8,000 UTF-8
bytes. Card summaries still have a 2,000-byte limit. `ask_deadlines`,
`mote_requests` and `escalations` enable deadlines, Mote request tracking and
escalation. Coordinate daemon upgrades with the owner; no capability failure
automatically restarts it. A newer `fray drive` still runs against a daemon that predates
controller detail: it warns once that the orphan check and the child shown in
`fray agents` are unavailable, and continues.

Updates, history, and inbox fan-out commit atomically before notification.
SQLite uses WAL and `synchronous=FULL`. A missed socket notification is recoverable
from durable state. Repeated changes coalesce into one pending entry per
conversation; history and explicit omission counts remain available.

```sh
fray --as codex --json wait --timeout none --selection involved
# Optional role-specific filter: p2 or more urgent, including linked objections.
fray --as reviewer --json wait --timeout none --kinds question,objection --min-priority p2 --addressed-to-me
fray --json watch --after 0 --reconnect
# Host-neutral, model-facing NDJSON; silent while there is no selected attention.
fray --as reviewer watch --attention --selection involved --reconnect
# Background-completion hosts can consume one packet, then explicitly rearm.
fray --as reviewer watch --attention --once --timeout 300
fray --json brief --as codex --budget 12000
fray stop
```

Retain `(store_id, cursor)` for stream replay. Inbox receipts are per card and
priority ordered; they are not stream cursors. Mutations support `--key` for
identical retries after an ambiguous connection failure. Reads never acknowledge.
`brief` has a hard UTF-8 byte budget and reports omitted items.

`wait --timeout none` blocks until selected pending attention arrives.
`wait --card 12,19` and `watch --attention --card 12,19` narrow to 1–16 positive
card IDs, intersecting all other filters. `inbox --card` uses the same predicate.
These are filters over existing deliveries, not subscriptions: `follow ID` first
when the agent is not involved. Muted and filtered-out receipts stay durable. Omit
`--after` to resume from durable per-agent acknowledgments; use the exact returned
receipt with `ack --receipts` after handling. Wait exits 0 on attention, 3 on quiet
timeout, 4 on daemon unavailability or busy admission, and 1 on other runtime errors. Finite waits
default to 300 seconds. `--kinds` and `--min-priority p0..p3` work identically for
inbox, wait and attention watch; filtered-out receipts stay pending.

Attention streams default to `involved` (or `FRAY_SELECTION`), recover pending
receipts at startup/reconnect, and never acknowledge on output. `--budget 4000`
bounds each complete NDJSON line, including its newline. Packets carry current
heads, message bodies, linked follow-up cards and exact receipts, with explicit
truncation/omission fields. A fixed `--settle-ms 100` batches bursts; priorities
0/1 bypass the window. Transport heartbeats stay off stdout. `--once` exits after
one packet; `--timeout` exits with status 3 on quiet expiry (errors use 1). Without
a timeout the listener runs until cancelled. `--after` belongs only to broadcast
watch and cannot be combined with attention mode.

`wait` and attention watch also support the inbox's `--addressed-to-me` and
`--unresolved` filters. Those deliberately narrow selection: addressed-only may
exclude answers to your outgoing questions, and unresolved-only excludes closure
notifications. Hidden receipts stay pending. `agents` reports an expiring
`listener` lease separately from the managed `controller`; armed means a transport
consumer is connected, not that a model is working or will answer promptly.

`fray --as reviewer doctor` checks the daemon, selected pending attention, listener,
and adapter-declared activation without starting a daemon or acknowledging anything.
A live socket and an unexpired host declaration do not guarantee a model response.
Hosts with small notification displays can consume `watch --attention --notification`:
each line is at most 768 bytes and points to an exact batch to fetch before handling.
The daemon admits at most 112 long-lived waits/watches among 128 clients. It lowers
those limits when the process has fewer file descriptors and reserves up to 16
connection slots for short operations; `doctor` shows the actual limits. Cancelled
finite and indefinite waits release their slots without the original deadline.

Claude, Codex and other hosts share this protocol. The optional Claude plugin
connects it to interactive notifications; generic `drive` remains available to
any host that consumes stdin and exits after a turn. See the
[host integration and qualification guide](integrations/README.md#host-neutral-attention-stream).

The trust boundary is one OS user. Identities are asserted, topics are routing,
and claims do not lock files. History currently grows without automatic pruning.
For a backup, stop Fray and copy the whole home directory, including any WAL/SHM
files. `start --normal` explicitly relaxes power-loss durability.

See `fray --help`, the [architecture](docs/ARCHITECTURE.md),
[integrations](integrations/README.md), and [validation](docs/VALIDATION.md).
MIT licensed. No package-registry release is claimed.
