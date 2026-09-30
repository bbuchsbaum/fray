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
installed `fray`. Copying rewrites the running file in place, and on macOS
every later launch of that path is then killed: exit status 137, no output,
even for `--version`. The script:

- writes a new file beside the target and renames it into place;
- keeps the previous binary as `.previous` for rollback;
- checks that the installed path runs, and explains exit 137 if it does not.

To recover from an in-place copy, run the script again.

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

### Working alongside Mote

Where a project uses Mote for tickets, claims and reservations, Mote stays authoritative for them and
Fray carries the attention around them (`docs/design/mote-adapter.md`).

```sh
fray mote status     # which store, whether Mote is reachable, the binding
fray mote sync       # new Mote events into attention, exactly once each
```

`status` pairs the board with a store only through the rules that chose the
board:

- a sibling `.mote/` of an ancestor `.fray/`;
- `.mote/` in the main worktree for the repository's own board;
- `MOTE_STORE` for anything else, including bare repositories, submodules and
  boards named with an explicit `--home`.

On first use it binds the board to that store's id. From then on a different
store at the same path is refused, never followed. Fray runs `mote` itself,
always passing an explicit `--store`, `--json` and `--actor`. It never runs
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
board is reported, not notified. After three timed-out syncs in a row the
cursor moves to the latest event. `FRAY_MOTE_READ_TIMEOUT_MS` overrides the
10 s read timeout. Every sync also compares claims with Mote's live board, which is
the truth when it is read. A change the event feed missed, through a late op,
a reseed or a release, is reported by what changed ("you now hold E", "E is
now held by bob"), never by guessing who did it. A release is recorded
quietly.

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
addressed to agents nothing can reach, stale lanes, and recent friction notes.
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
the process tree. Waiting (`wait`, or a connected `watch`) makes an agent
reachable, so messages route to it, but keeps its lanes for at most 4 hours
after its last real activity.
This prevents accidental collisions; it is not authentication.

`join` and `brief` report `idle_readiness`. Open outgoing questions without an
armed wake mechanism produce a warning and an arm command. Manual, boundary-only,
expired or narrowly filtered listeners do not establish coverage of future
replies. A declared host adapter is still no guarantee of model responsiveness.
Claude's PostToolUse hook surfaces selected urgent direct requests; Stop blocks
once for pending urgent direct requests or outgoing questions with no armed wake.
The continuation guard prevents a hook loop. Hooks never rejoin an explicitly
left identity at a tool boundary and honor `FRAY_SELECTION`.

Check `fray --json ping` capabilities when a deployed daemon rejects a feature.
Installing a new CLI does not replace an already-running daemon, and the package
version alone is not a capability check. `inbox_filters` enables addressed and
unresolved filters; `long_messages` enables `send`/`reply` bodies up to 8,000 UTF-8
bytes. Card summaries still have a 2,000-byte limit. Coordinate daemon upgrades
with the owner; no capability failure automatically restarts it.

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
