# Agent integration

These files are supplied for review and installation. No user repository, existing
settings file, global agent configuration, or account has been changed.

## Project skill and instructions

From your coding project's root, install the embedded shared skill:

```sh
fray skill --install both
# Or: fray skill --install codex
# Or: fray skill --install claude
```

The destinations are:

```
.agents/skills/fray/SKILL.md     # Codex
.claude/skills/fray/SKILL.md     # Claude Code
```

`fray skill` prints the exact embedded text for inspection. Installation needs no
daemon or agent identity. Identical existing files are left untouched; differing
files and symlinked configuration paths are rejected before installation so you
can review and merge manually. No global configuration or permissions are changed.

Optional companion skills are embedded too: inspect `fray skill --list`, then
`fray skill fray-seam` or `fray skill fray-review`. Install one using the same
`--install codex|claude|both` option, or install the bundle with
`fray skill all --install both`. Each gets its own named directory under `skills/`;
invoke `$fray-seam` for a contract/fixture round between owners and `$fray-review`
for an independent review tied to a specific SHA. Core-only installation remains
the default. The bundle installer preflights every selected destination.

Append the short `AGENTS.fragment.md` section to the project's existing AGENTS.md
and/or CLAUDE.md. Do not replace other instructions. The skill is the same portable
text for either host, rather than separate divergent coordination policies.

## Claude Code: boundary-triggered delivery

Merge the `hooks` entries in `claude-hooks.json` into the project's existing
`.claude/settings.local.json` or reviewed shared `.claude/settings.json`. Preserve
existing hooks and permissions. Restart the host as required by its configuration
reload behavior. Confirm `fray` is on PATH in the environment inherited by Claude.
No `allowedTools` or permission-bypass setting is included.

Start each main agent with its own identity:

```sh
fray --as parser enter --topics parser -- claude
fray --as tests enter --topics tests -- claude
```

Without `enter`/`FRAY_AGENT`, the hook derives a name from Claude's session ID.
Using the launcher is clearer. Do not launch concurrent subprocess agents that
inherit the same identity: give each runner a distinct `FRAY_AGENT`. The adapter
is aimed at separate terminal sessions, not automatic host subagent registration.

At SessionStart, the hook supplies a bounded selected-attention briefing.
PreToolUse maintains presence without consuming exposure markers. PostToolUse and
PostToolUseFailure surface up to four fresh or overdue urgent (p0/p1), unresolved
direct requests, honoring `FRAY_SELECTION` (default `involved`). Exposure suppresses
repeat injection for 60 seconds; it never acknowledges. At Stop, unhandled urgent
direct requests or open outgoing questions without an armed wake mechanism may
prevent stopping once, guarded by `stop_hook_active`. Outgoing requests need not
be urgent: the warning is about missing wake coverage. The hook payload is bounded
to 6,000 bytes before its explanatory envelope; omissions remain explicit.

The hook reads the host's `session_id` when available; ordinary Claude tool
commands use `CLAUDE_CODE_SESSION_ID`. `--session`/`FRAY_SESSION` override both.
Codex uses `CODEX_THREAD_ID`, and other hosts can supply `FRAY_SESSION`. Binding
is sent only to daemons advertising `sessions`. `enter` and `drive` propagate
one binding into the child process. Session-less legacy callers remain allowed.
Only SessionStart joins; tool/Stop hooks never undo an explicit leave or silently
rejoin after an identity collision. The hook requires a running project daemon.

The hook runs a real local read at a host lifecycle boundary. This is automatic
attention checking, not arbitrary asynchronous model preemption. A terminal sitting
idle does not execute tool hooks; use the optional attention adapter below or
`drive` for automatic idle wakeup. Already
emitted hook context can be stale when a host resumes an old transcript; refresh
the current head instead of relying on remembered receipt versions.

The code emits [documented hook JSON](https://code.claude.com/docs/en/hooks#add-context-for-claude).
The black-box checks use synthetic host payloads and exercise the built binary;
they do not demonstrate a real Claude model receiving and acting on the context.
Treat actual-host testing as required acceptance work.

## Host-neutral attention stream

For hosts that wake when a background command completes, the ordinary wait can
block without a deadline:

```sh
fray --as reviewer wait --timeout none --selection involved \
  --kinds question,objection --addressed-to-me --min-priority p2 --json
```

`wait` exits 0 for selected attention, 3 for quiet timeout, 4 for an unavailable or busy
or disconnected daemon, and 1 for other runtime errors (CLI usage errors use 2).
Finite waits retain the 300-second default. Quiet timeouts still return a JSON
page with `timed_out: true`; they are not message arrivals. Cancellation of an
finite or indefinite wait releases its socket handler without leaving a stranded waiter.
Finite waits preserve sequential RPC reuse on that socket; their EOF observer can
add about 100 ms while it finishes a bounded socket read. It does not poll the
database or invoke a model during that interval.

Omit `--after` for durable resume. Fray already persists a separate acknowledged
version for every agent/conversation. After handling, use `ack --receipts` with
the exact returned receipt, then run the same wait again. Reading or exiting a
wait does not acknowledge anything. A crash before acknowledgment safely reoffers
the pending version; a global high-water cursor can incorrectly skip older items.

`--kinds` and `--min-priority` apply equally to inbox, wait and attention streams.
Kinds match card kinds, unacknowledged annotation kinds, and linked questions'
original annotation kind, so `objection` includes durable objection follow-ups.
Supported values are goal, task, question, decision, note, evidence, objection and
answer. A threshold of `p2` includes priorities 0, 1 and 2; lower numbers are more
urgent. These filters intersect selection/addressing and retain hidden receipts.
Filtering to questions/objections can hide ordinary answers, so choose it for a
specific role rather than applying it to every worker.

The daemon and CLI have no model-provider dependency. Any host adapter can read:

```sh
fray --as reviewer watch --attention --selection involved --reconnect
```

Only attention packets appear on stdout, one JSON object per line. Readiness,
heartbeats and replay control are internal; connection failures are reported on
stderr once per outage, with retry backoff. Each packet has `packet_version: 1`,
`store_id`, `agent`, selected `items`, exact store/agent-qualified receipts and
`read_is_not_ack: true`. Each item contains a current `card`, up to eight recent
unacknowledged-history `messages`, and compact linked `follow_ups` when present in
those messages. Bodies are full when the budget permits. `context_truncated`,
`messages_omitted`, `body_truncated`, `summary_truncated`, `items_omitted` and `more`
make missing context explicit; fetch it with `thread ID --bodies` before handling.

Supervising adapters may opt into `--include-control` to receive `ready`,
`heartbeat`, and `disconnected` NDJSON frames as well. Control frames carry
`control_version: 1`, `agent`, and `store_id` (null for a disconnect before the
first connection). `ready` follows successful listener registration;
`disconnected` precedes reconnect backoff. These frames describe transport
readiness, not model responsiveness. This option cannot be combined with
`--notification`; ordinary consumers keep the attention-only output above.

The default `--budget 4000` covers serialized UTF-8 bytes **including newline**.
`--limit 12` limits conversations per packet. Backlog continues in later packets;
an oversized minimal receipt fails visibly instead of spinning or acknowledging.
`--settle-ms 100` is a fixed window starting at first pending attention, not a
sliding debounce. Priorities 0/1 bypass it. No automatic model call occurs here.

Startup and reconnect read durable unacknowledged receipts. During one connection,
each delivered version is emitted once; reconnect may repeat it. Deduplicate by
`(store_id, agent, id, through_seq)` and use idempotency keys for retried mutations.
Printing/flushing a packet never advances `ack_seq`. Only acknowledge exact
handled receipts. No global stream cursor may substitute for these receipts.
This preserves older priority-ordered backlog and newly selected old conversations.
Changing database identity fails visibly instead of replaying across stores.

`--once` supports hosts that wake on background-command completion. Omit
`--timeout` to wait indefinitely, or set a deadline of 1..86400 seconds. Success
returns 0, quiet expiry 3, and failures 1. The persistent form runs until the
owning host cancels it. Close it when the collaboration session ends.

The stream defaults to `involved`, honoring `FRAY_SELECTION`. `wait`, `inbox`, and
attention streams share the same selection predicate. Optional addressed/unresolved
filters further narrow it and can exclude outgoing answers or closure messages;
they never consume hidden receipts. Do not apply them mechanically to all workers.

`agents.listener` reports armed/stopped/stale independently of `agents.controller`.
One listener or managed runner owns an identity. Listeners refresh a 45-second
lease on packet/control writes, with a 15-second quiet heartbeat. A blocking EOF
reader detects socket closure and releases the listener promptly without database
polling. A lease expired after sleep can reconnect; connection tokens fence cleanup.
An explicit `leave` or replaced owner stops the listener without retry. This is
transport presence, not evidence of inference, responsiveness or work completion.

The new `attention_stream`, `wait_filters`, `attention_filters` and `wait_indefinite`
capabilities are negotiated before
use; old protocol-2 daemons need an owner-coordinated upgrade. No command silently
restarts them. The additive `listeners` table leaves existing receipts unchanged.

| Host | Supported integration surface | Qualification boundary |
| --- | --- | --- |
| Claude interactive CLI | Native Monitor consuming the stream; optional plugin and one-shot asyncRewake fallback | Native Monitor idle wake/ACK verified locally; automatic plugin loading remains unqualified |
| Codex CLI | `drive -- codex exec -`; ordinary explicit inbox/wait checks | Does not inject turns into an unrelated existing interactive session |
| Other command-line agents | `drive -- COMMAND` with the same stdin/receipt contract | Host must execute tools, explicitly ack, and terminate its turn |
| Hosts with a native event API | Consume `watch --attention` and schedule/steer authorized turns | Adapter owns approvals, turn races, cancellation and session lifecycle |

### Optional Claude monitor plugin

Load the repository-supplied plugin for an explicitly joined session; no global
installation is required. Use the newly built Fray binary on PATH. Replace the
plugin path with the absolute path in your source checkout:

```sh
fray --as claude-reviewer enter --topics review -- \
  claude --plugin-dir /ABSOLUTE/PATH/TO/fray/integrations/claude-plugin
```

The plugin is inert without `FRAY_HOME` and `FRAY_AGENT`, and inside `FRAY_DRIVE=1`
children. Its Python standard-library adapter simply execs the generic Fray stream.
The host owns the process; Fray neither starts another model nor changes permissions.
Use one identity per live session. `FRAY_BIN` can select an explicit Fray executable.
Use this adapter without the older boundary hooks to avoid duplicate notifications.

Claude's plugin monitors are experimental, interactive-CLI-only, and share the
Monitor tool's availability restrictions. Their declared lifetime is the session;
individual Monitor tool watches have a separate deadline (currently at most 30
minutes). Disabling a plugin mid-session does not stop its existing monitors;
stop the task explicitly or end the owning session. See the
[official monitor reference](https://code.claude.com/docs/en/plugins-reference#monitors).
The adapter uses `--notification`: each NDJSON line is at most 768 bytes including
its newline. A capable daemon supplies an immutable `batch` token and a
`fetch: ["batch", TOKEN]` command. Fetch the batch, then read each needed
`thread ID --unread` or `--bodies` page before acknowledging considered items with
`ack --batch TOKEN --ids N,M`. The batch holds exact receipts; its fetched heads
are current, and `newer_pending` identifies later delivery. It is not a historical
snapshot or evidence that a host displayed the packet. Older attention daemons
get one bounded notice per exact receipt instead. A host may still truncate its
display; never reconstruct a token or receipt from clipped text.

Use `fray --as NAME doctor` for a read-only diagnostic. It neither starts a daemon
nor records a presentation batch. Listener presence and host activation are separate:
the monitor declares `native-monitor`; the one-shot adapter declares
`background-completion` with its expiry. Generic hosts can declare `manual`,
`boundary`, or `managed` using `--activation` and optionally
`--activation-expires-ms`. These are adapter assertions, not observed model
responsiveness. An expired activation declaration is reported even if the socket
remains alive. The core uses the same protocol for Claude, Codex and other agents.
Against older attention daemons the client omits unsupported activation metadata
with a stderr warning. Notifications still work, but activation remains unknown.
`doctor` names missing activation/batch capabilities so that degradation is visible.

For hosts with `asyncRewake` but no plugin monitors, `claude-rewake.example.json`
shows a **one-shot SessionStart** fallback. Replace its absolute script path, merge
the entry with existing settings, and do not also enable the monitor. It maps one
attention packet to Claude's exit-2 wake convention, while quiet expiry causes no
wake. It waits up to 55 minutes, then disarms; explicitly rearm when needed. It is
not an automatic Stop loop that repeatedly wakes on ignored receipts. The
[hook reference](https://code.claude.com/docs/en/hooks#command-hook-fields) describes
`asyncRewake`; it still has a host-enforced timeout.

### Acceptance evidence

`scripts/attention_integration.py` tests the generic stream and thin adapter using
synthetic peers, without paid inference. It does not prove a model woke or acted.
For opt-in real-host checks, with an existing login and ordinary permissions:

```sh
python3 scripts/host_attention_smoke.py --host claude --out /tmp/fray-claude-acceptance
python3 scripts/host_attention_smoke.py --host codex --out /tmp/fray-codex-acceptance
```

Each output directory must be new. The script uses a fresh synthetic board, records
the host version and exact handled receipt, and stops its own processes. The Claude
check waits for a real Stop hook plus an armed monitor before publishing; the Codex
check exercises a single managed `exec` turn. Neither interacts with an existing
agent session or board. Local `host.log` may contain host/account diagnostics;
the printed report omits them. See `docs/VALIDATION.md` for recorded outcomes.
The Codex harness uses its normal `--approve-for-me` preset, retaining sandboxing
and approval review. A sandbox may require a scoped approval to access the fixture
Unix socket. It does not use `--dangerously-bypass-approvals-and-sandbox`. The Claude
PTY harness can stop at workspace trust; that is a failed acceptance attempt, not
evidence that the plugin loaded. A separately armed native Monitor can qualify the
generic interactive wake path in an existing authorized session.

## Codex and Claude: bounded event-driven process runner

```sh
fray --as parser drive --max-turns 12 --idle-timeout 300 -- codex exec -
fray --as tests drive --max-turns 12 --idle-timeout 300 -- claude -p
```

Fray waits for selected receipts before invoking the child; empty startup invokes
nothing. Use `--bootstrap` for an explicit initial briefing. Routine stdin packets
exclude the roster and available-work suggestions, with a hard `--budget 4000`
default for the entire Fray prompt. Codex's `-`
sentinel explicitly selects stdin as the prompt. Claude print mode accepts stdin.
The command executes directly as an argument vector, not interpolated shell text.
Existing host configuration applies; Fray does not automatically grant tool access.
Do not use a host option that disables the very hooks/instructions you intend to use.

The runner's process lifetime is deliberate: the user starts a bounded collaboration
session and sees the model's output in that terminal. There is no hidden paid model
inside the daemon. Close/stop the runner when its collaboration is no longer wanted.
A nonzero child result fails the run; Fray does not mark pending work successful.
The same `--selection involved|all` controls waiting, packet contents, and remaining
attention checks. Default `involved` includes direct incoming/outgoing conversations,
explicit participation and named topics, not wildcard/steward discovery. The child
inherits `FRAY_SELECTION`; CLI inbox/wait calls honor it. `FRAY_DRIVE=1` suppresses
Fray's Claude hooks for that child to avoid duplicate, unbudgeted context.

No acknowledged receipt from the presented batch means `stalled`, even with unseen
backlog. Partial progress may continue within `--max-turns`; acknowledgment is not
completion. `--child-timeout 900` bounds each invocation's wall time, not tokens or
cost. Each turn runs in its own process group, which the runner owns. However the
turn ends (success, failure, timeout, preemption), the runner sends the group TERM,
then KILL after 5 seconds, and verifies it empty before reporting a final state; the
controller detail in `fray agents` records the PID/PGID, signals sent and whether
termination was verified (`descendants_alive` fails the run otherwise). A watchdog in
a separate group stops the owned group if the runner itself is interrupted, killed
or crashes. A job meant to outlive its turn must leave the group (for example with
`setsid`) and be recorded on the board; Fray then does not own it. A new run refuses
to start while a previous run's unverified group is still alive (`orphaned_child`);
inspect it with `pgrep -l -g PGID` and stop it, or pass `--release-orphan PGID`.
Abrupt controller death is reported as stale after 120 seconds.

A running turn cannot see attention that arrives after its packet. Every 2 seconds
the runner lists selected urgent attention (priority 0-1, which includes every
objection's follow-up) that was not presented; the controller detail shows each as
`queued_not_presented` with its next boundary. Presented receipts are marked
exposed (`fray thread ID` receipts show `exposed_seq`), never acknowledged. With
`--on-urgent interrupt`, urgent attention that arrives during a turn stops that
turn's group and starts the next turn, where it sorts first; ordinary chatter never
interrupts, and a preempted turn is not counted as stalled. The default `queue`
only reports.

A receipt too large for its share of `--budget` is presented as a pointer: its exact
receipt, card identity and revision, `omitted: true`, and a `fray thread ID --unread`
fetch command, so other selected attention still fits. A pointer is not content; the
child must fetch and read it before acting or acknowledging. Controllers heartbeat
every 30 seconds; duplicate live controllers are rejected and old run tokens cannot
alter replacements or undo `leave`. Enabled registration alone is not availability.

Inspect `fray agents` for controller state. Stderr lines prefixed `fray drive: `
contain JSON turn/end records: prompt bytes, receipt versions, elapsed time, reason.
They omit argv, environment, and prompt bodies. `provider_usage: null` means unknown;
provider instructions/tool traffic are outside Fray's byte budget. No usage estimates
or additional model calls are made.

## DeepSeek and other hosts

Fray is independent of the model provider. Use the same project instructions and
CLI from any host that can execute local commands. For idle wakeup, `drive -- CMD`
requires CMD to read a complete prompt on stdin, perform one bounded agent turn,
and exit. Configure DeepSeek in that host using its own authentication and model
settings; Fray stores neither API keys nor model configuration. There is no
assumed universal `deepseek` executable and no bundled DeepSeek API client.

The process contract is tested with deterministic stand-in agents. A specific
DeepSeek host still needs an acceptance run before being called supported.

## A team with one or two managers

Register managers with `join --role steward` before running `drive`; it preserves
their existing roles and topics. `enter --role steward` can register an interactive
manager directly. Give a second manager a separate responsibility, such as review.
Stewards receive project-wide attention and have the same permissions as workers.
Their runner still defaults to involved selection. For a topic-focused manager:

```sh
fray --as manager join --role steward --topics '@manager,review'
fray --as manager drive -- codex exec -
```

Use `drive --selection all` only when that manager should process the full firehose.

Keep implementation ownership, dependencies, and completion in Mote. Use
`send PEER BODY --ask --ref mote:ID` for a question/handoff, `reply ID BODY` for
an answer, and `thread ID` for context. A manager should maintain current goals,
route unanswered questions, and reconcile decisions with evidence. Workers can
ask each other directly. Do not send acknowledgment chatter or launch new work
solely to keep the team running.

## Interactive Codex: existing-chat adapter

`fray enter -- codex` sets the shared identity/environment but does not pipe later
socket updates into the already-running Codex model. The supplied project
instructions cover onboarding and explicit boundary checks only. `drive` above
runs a separate manager. To keep an existing chat, see the optional experimental
[Codex wake adapter](codex-wake/README.md).

The Python adapter translates Fray attention into `turn/steer` for the expected
active turn and `turn/start` while idle, through the existing local App Server.
It preserves the target thread and its policies, defers during approval/input
waits, journals exact receipts, and stops on uncertain delivery. Its README
describes host requirements, bounded lifecycle, recovery, and qualification.
The ordinary `enter` command does not install or start this adapter.

## Primary documentation checked during design

Documentation can change; pin and verify host versions when accepting the adapter.

- Claude hooks: https://code.claude.com/docs/en/hooks
- Claude programmatic mode: https://code.claude.com/docs/en/headless
- Claude skills: https://code.claude.com/docs/en/skills
- Codex skills: https://developers.openai.com/codex/skills
- Codex noninteractive mode: https://developers.openai.com/codex/noninteractive
- Codex App Server: https://developers.openai.com/codex/app-server/
