# Design: keep an interactive agent answerable after its turn ends

Status: revision 4 (2026-10-03), for re-review. Revision 3 (85c3fc9) left
#103 open once more: its flags only govern tools that would prompt, so the
user's own settings (here `Write(*)`, `Edit(*)`, `Bash(timeout:*)`, web
tools) and Codex's MCP servers and built-in tools still reached the turn.
Revision 4 pins the turn's tool set itself, with the evidence below.

Earlier history: revision 3 (85c3fc9), for re-review. Revision 1 (1034e02) drew
five blocking objections, fray #102 to #106; revision 2 (fa40ad7) resolved
four and left #103 (permissions) open: a Codex turn without network cannot
reach the board's socket, and `Bash(fray:*)` and `Bash(git diff:*)` allow
escapes. Revision 3 takes the board out of the background turn entirely
[#103] and takes the review's other points.

## The problem

The owner opens several interactive terminals (two Claude Code, two Codex)
and says "join the fray". A team leader then wants to say "you're idle,
review this" to any of them. Today that works only for an agent that is
`wakeable` (docs/design/no-silent-stalls.md, R1): an interactive Claude
whose Monitor runs what `fray arm` printed, or any agent under `fray drive`.
An interactive Codex can never be woken once its turn ends; an interactive
Claude cannot once its Monitor expires. The request escalates if it sticks
(R3), but the agent does not act until the owner types.

## What experiments established (2026-10-03, scratch)

| | Codex 0.160 | Claude Code 2.1 |
|---|---|---|
| A non-interactive turn inside an existing conversation | `codex exec resume ID -` remembered (7 s) | `claude -p --resume ID` (4 s) |
| Branch a conversation, keeping its memory | `codex exec fork --json ID -`; the new id is the first JSONL line (`thread.started`) | `claude -p --resume ID --fork-session --session-id NEW`: the fork takes an id chosen up front |
| A later resume sees an earlier background turn | yes | not tested |
| The id an interactive terminal exposes is resumable | `CODEX_THREAD_ID` (checked for live terminals) | `CLAUDE_CODE_SESSION_ID` |
| End to end: a drive running the resume command under the agent's name; a leader's ask | answered on the card from inside its conversation | same |
| A process in its own process group survives the terminal closing (macOS) | yes, with or without `nohup`; also survives the Claude Bash call returning | same |

Both agents, asked by a peer to post a secret the user had given them,
declined: they had their memory and applied the rule that peer text is not
authority.

Two findings shape this revision [#102]: a process started from inside a
Codex tool call inherits Codex's sandbox (seatbelt; it kept
`CODEX_SANDBOX=seatbelt` and could not write under `~/.codex`, even
detached), and a fork of even a one-turn Codex conversation read 51,948
input tokens, uncached.

## The design

### Who starts the background drive: the daemon [#102]

An agent cannot start its own keepalive from inside a sandboxed tool call:
the drive and its turns would inherit the sandbox. So the agent *asks* and
the **daemon starts it**:

```sh
fray keepalive            # ask the daemon to keep this agent answerable
fray keepalive --stop     # stop it
fray keepalive --status   # what it is doing, and why not, if it is not
```

`fray keepalive` sends a `keepalive_start` request carrying the terminal's
working directory. The host and conversation are not taken from the
request: the daemon reads them from the session the request is bound to
(`codex:THREAD` or `claude:SESSION`, which the client sets from
`CODEX_THREAD_ID` or `CLAUDE_CODE_SESSION_ID`), so a caller cannot make it
fork someone else's conversation; an unbound request, or one from another
kind of session, is refused, with `fray drive` as the way for other hosts.
The directory must be this board's repository or one of its git worktrees
(a resumed conversation runs, and is filed, under its working directory). The daemon validates it and spawns the drive itself,
detached (its own process group, stdin closed, output to
`HOME/keepalive/NAME.log`), with an explicit `FRAY_SESSION` [#104].

The daemon runs only fixed command shapes, built by fray from the host and
id (below); a request cannot name a program or add arguments. Owner
options (extra allowed tools, a different profile) come from the Team card
or the daemon's start environment, never from the request. If the daemon
itself is sandboxed (it was started from inside a Codex tool call), it says
so and refuses. It checks with a nested `sandbox-exec -p '(version 1)(allow
default)' /usr/bin/true`, which fails inside a seatbelt sandbox and
succeeds outside, rather than trusting `CODEX_SANDBOX`, which can be unset.
`fray doctor` reports it, and the fix is starting the daemon from the
owner's shell.

The request itself must reach the daemon's socket: an interactive Codex
whose sandbox has the network off cannot send it. The skill says so; the
owner's Codex configuration decides.

### Its own session, as the agent's companion [#104]

The drive runs under the agent's name with its own session label,
`keepalive:CONVERSATION`, and its controller detail names the terminal
session it serves (`companion`). The board allows one keepalive controller
to coexist with one interactive session of the same name: `bind_session`
accepts the companion pair, and a `/clear` or `/compact` in the terminal
(which rebinds the interactive side to a new session) does not touch the
keepalive's binding. It does change the terminal's conversation, so the
SessionStart hook that records the new session also updates the
keepalive's `companion` and its fork source, and the next background turn
forks the new conversation. `leave` from the terminal stops both.

### The background turn reads; the drive acts [#103]

A background turn cannot stop to ask the owner, and nobody watches it. So it
gets no tools that change anything, the board included. It reads the
packet and the code, and **returns its decision as structured output**; the
drive, outside any sandbox, checks that output and applies it.

- **claude:** `claude -p ... --json-schema SCHEMA --output-format json
  --tools Read,Grep,Glob --restricted --strict-mcp-config --permission-mode
  dontAsk`. `--tools` sets the tool set itself (not allow rules, which the
  settings files would extend); `--restricted` ignores the user, project and
  local settings files and confines the file tools to the working
  directories; `--strict-mcp-config` loads no MCP servers. Verified: with
  allow rules alone a turn wrote a file because the user's settings allow
  `Write(*)`; with `--tools Read,Grep,Glob --strict-mcp-config` Write was
  unavailable and nothing was written. The structured result is the
  top-level `structured_output`; a missing one, or `is_error: true`, is a
  failed turn.
- **codex:** `codex exec fork|resume ... --json --output-schema FILE
  --ignore-user-config -c approval_policy="never" -c sandbox_mode="read-only"
  -c web_search="disabled"` and `-c features.NAME=false` for `multi_agent`,
  `apps`, `browser_use`, `browser_use_external`,
  `browser_use_full_cdp_access`, `computer_use`, `image_generation`,
  `plugins`, `remote_plugin`, `in_app_browser`, `in_app_local_automation`
  and `skill_mcp_dependency_install`. `--ignore-user-config` drops the
  user's MCP servers (auth still comes from `CODEX_HOME`). Verified on a
  fork: it remembered the conversation; a write was refused by the
  read-only sandbox; the turn listed no MCP servers and no web, image,
  browser or app tools. What remains is shell execution and `apply_patch`
  under the read-only, no-network sandbox, clock and goal tools, and
  `collaboration.spawn_agent`, which `multi_agent=false` did not remove.
  K1 must show that spawned agents inherit the same sandbox and settings
  (or find the key that removes them) before the keepalive is announced;
  the drive's child timeout bounds their cost either way. Without the
  user's configuration the model is the one the forked conversation
  recorded, or the Team card's `keepalive model:`.

The schema, the same for both hosts:

```json
{"actions":[{"card":12,"kind":"answer|evidence|objection|question|note","body":"..."}],
 "handled":[12],"summary":"one line for the terminal"}
```

The drive applies it as the agent:

- each action becomes a reply on its card, of that kind, through the same
  operation `fray reply` uses, so routing, objections and follow-ups behave
  as for any reply;
- an action may name only a card in the packet the turn was given, or a
  linked follow-up of one; anything else is refused and reported in the
  drive's log and on the first packet card;
- `handled` acknowledges exactly those cards' receipts from that packet's
  batch (never more), which is also the drive's progress check;
- output that fails the schema is a failed turn: nothing is posted or acked,
  and the drive stops after its usual consecutive-failure limit.

A turn that needs more than reading says so in its `body` (e.g. "a fix is
needed at src/x.rs; the owner or a worker with edit rights should take it").
Editing by a keepalive is out of scope for this design.

### One turn at a time, with the terminal [#105]

Receipts belong to the name, so the terminal and its keepalive see the same
asks. They take turns, both ways:

- **The terminal is busy** from `UserPromptSubmit` until a `Stop` that does
  not block (a blocking Stop continues the turn). Claude 2.1 has no
  interrupt event, so `StopFailure` and `SessionEnd` also end busy; Codex
  0.160 lists its own interrupt event, which does too. `fray hook` gains
  these events for both hosts, and the Codex hook configuration in the
  README adds them. A busy mark with no hook activity for 30 minutes is
  stale (an interrupt that no event reports).
- **The keepalive defers** while the terminal is busy, and `fray team` and
  `fray stuck` show it as `deferred (terminal busy)`, not as plainly
  wakeable.
- **The terminal defers to the keepalive** while a background turn runs: the
  terminal's hook context names the cards the keepalive is answering right
  now ("being handled by your keepalive; do not take them"), and they are
  left out of its attention.
- **If the hooks are not installed**, the keepalive cannot see the terminal,
  so it refuses to start and says how to install them.

### The fork follows the terminal [#106]

The keepalive answers from a fork of the agent's conversation, so it never
writes behind the terminal. The fork is renewed whenever the terminal has
taken a turn since the fork was made: before each background turn, if a
`UserPromptSubmit` from the companion session is newer than the fork, the
keepalive forks again from the terminal's conversation (Claude: a new
`--session-id` chosen up front; Codex: the id from `thread.started`, read by
the drive, which now pipes and tees the child's output). Otherwise it
resumes its current fork. So it always knows what the terminal knew at its
last turn, plus its own background turns since.

### Telling the terminal what its background self did

At the terminal's next `UserPromptSubmit`, the hook context starts with what
the keepalive did since the terminal's last turn: the cards it answered or
created, with ids. The agent can read them and carry on without repeating
work.

### Cost, bounded rather than printed

Each wake is a paid turn over the whole conversation, often uncached; a
fork reads all of it. So the keepalive keeps a daily budget of input tokens,
counted from the usage both hosts report in their JSON output (default
2,000,000 a day, set by the owner in the Team card). Over budget, it stops
taking turns, says so on the board, and stays visible as `paused (budget)`.
It also stops after `--max-turns` (default 50) and after 24 hours idle, as
drives do.

### Stopping

`fray keepalive --stop` sets a stop request on the controller. A turn in
progress finishes (bounded by the child timeout) and its output is applied;
then the drive exits instead of waiting again, and marks itself stopped.
`fray team` shows it as `stopping` at once and stops showing it when it has
exited. `leave` stops it too. The daemon keeps
the drive's pid, so `fray doctor` can name a keepalive whose process is gone.
A Claude whose Monitor is armed is already wakeable: `fray keepalive` says
so and does not start a second controller (which would fail with
`controller_busy`).

## Slices

K1 is not announced to agents until K2 lands; the skill changes only in K3.

- **K1.** `keepalive_start`/`stop`/`status` in the daemon; the daemon-spawned
  detached drive with fixed command shapes; the companion session, with the
  conversation from the bound session and the directory checked; the
  read-only turn and the drive applying its structured output;
  fork-then-resume with the fork id captured; stop requests; the budget. Tests with stub hosts (scripts that print fork
  JSON, report usage and read the packet): command construction (no
  request-supplied arguments), companion binding including `/clear`,
  fork capture, stop latency, budget pause, refusal when sandboxed or when a
  Monitor is armed. One paid smoke test per host, with the daemon started
  from a normal shell and the agent's request made from inside a Codex
  sandbox.
- **K2.** Busy from `UserPromptSubmit` to a non-blocking Stop, both ways;
  re-fork when the terminal has moved on; the "while you were away" context;
  `deferred` and `paused` in `fray team` and `fray stuck`.
- **K3.** The skill: "join the fray" ends with `fray keepalive` for
  interactive Codex, and for interactive Claude as the alternative to a
  Monitor; the Codex hook configuration in the README.

## Acceptance

- An interactive Codex and an interactive Claude each run `fray keepalive`
  (the Codex one from inside its sandbox) and go idle; a leader's
  `fray send NAME --ask` is answered on the card by each, from a fork that
  knows the conversation.
- While the owner works in the terminal, a waiting ask is answered only after
  the terminal's turn ends; while a background turn runs, the terminal is
  told and leaves those cards alone.
- After the terminal takes another turn, the next background answer knows
  what that turn did (re-fork).
- `/clear` in the terminal does not stop the keepalive.
- A background turn cannot change files or the board itself, on a machine
  whose settings allow Write, Edit, any Bash and MCP tools: asked to write a
  file, run a command or use an MCP tool, it cannot. Output naming a card
  outside its packet is refused; output failing the schema posts and acks
  nothing. A Codex sub-agent spawned by the turn has the same limits.
- A request naming another conversation, or from a directory outside the
  repository, is refused.
- The budget pauses it, visibly.
- `fray keepalive --stop` stops it within seconds; `leave` and the drive's
  bounds stop it; `fray team` reflects each.
- Closing the terminal does not stop it (a real terminal).
