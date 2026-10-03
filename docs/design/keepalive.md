# Design: keep an interactive agent answerable after its turn ends

Status: revision 2 (2026-10-03), for re-review. Revision 1 (1034e02) drew
five blocking objections, fray #102 to #106; each is addressed below and
marked [#102] and so on.

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

`fray keepalive` sends a `keepalive_start` request carrying the agent's
host and conversation id, read from `CODEX_THREAD_ID` or
`CLAUDE_CODE_SESSION_ID` (neither set: refused, with `fray drive` as the
way for other hosts). The daemon validates it and spawns the drive itself,
detached (its own process group, stdin closed, output to
`HOME/keepalive/NAME.log`), with an explicit `FRAY_SESSION` [#104].

The daemon runs only fixed command shapes, built by fray from the host and
id (below); a request cannot name a program or add arguments. Owner
options (extra allowed tools, a different profile) come from the Team card
or the daemon's start environment, never from the request. If the daemon
itself is sandboxed (it was started from inside a Codex tool call), it says
so and refuses; `fray doctor` reports it, and the fix is starting the daemon
from the owner's shell.

### Its own session, as the agent's companion [#104]

The drive runs under the agent's name with its own session label,
`keepalive:CONVERSATION`, and its controller detail names the terminal
session it serves (`companion`). The board allows one keepalive controller
to coexist with one interactive session of the same name: `bind_session`
accepts the companion pair, and a `/clear` or `/compact` in the terminal
(which rebinds the interactive side to a new session) does not touch the
keepalive's binding. `leave` from the terminal stops both.

### Permissions: a narrow, pinned profile [#103]

A background turn cannot stop to ask the owner, and nobody watches it. The
default profile is for answering and reviewing, not for editing the owner's
working tree:

- **claude:** `--permission-mode dontAsk` (so the settings' `defaultMode`,
  e.g. `auto`, does not apply) with `--allowedTools` limited to `Read`,
  `Grep`, `Glob`, `Bash(fray:*)`, `Bash(mote:*)`, `Bash(git log:*)`,
  `Bash(git diff:*)`, `Bash(git show:*)`, `Bash(git status:*)`;
- **codex:** `-c approval_policy="never"`, a read-only sandbox, network off,
  with only the board's socket and Mote store writable if Codex's sandbox
  configuration allows naming them (K1 verifies; if it does not, the turn
  runs read-only and posts its answer through the drive, which writes the
  card from outside the sandbox).

A turn that needs more says so on the card and stops. The owner may widen
the profile in the Team card (`keepalive: edit`), which also requires the
keepalive to hold a Mote reservation before editing; two keepalives never
edit one worktree at once.

### One turn at a time, with the terminal [#105]

Receipts belong to the name, so the terminal and its keepalive see the same
asks. They take turns, both ways:

- **The terminal is busy** from `UserPromptSubmit` until a `Stop` that does
  not block (a blocking Stop continues the turn) or an interrupt. Both hosts
  emit these events (Codex 0.160 lists them); `fray hook` gains
  `UserPromptSubmit` for both hosts, and the Codex hook configuration in the
  README adds it. A busy mark with no hook activity for 30 minutes is stale.
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

`fray keepalive --stop` sets a stop request on the controller; the drive
exits at its next wait (within seconds) and marks itself stopped, and
`fray team` stops showing it at once. `leave` stops it too. The daemon keeps
the drive's pid, so `fray doctor` can name a keepalive whose process is gone.
A Claude whose Monitor is armed is already wakeable: `fray keepalive` says
so and does not start a second controller (which would fail with
`controller_busy`).

## Slices

K1 is not announced to agents until K2 lands; the skill changes only in K3.

- **K1.** `keepalive_start`/`stop`/`status` in the daemon; the daemon-spawned
  detached drive with fixed command shapes; the companion session; the
  pinned permission profiles; fork-then-resume with the fork id captured;
  stop requests; the budget. Tests with stub hosts (scripts that print fork
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
- An action outside the profile is refused, and the turn says so on the card.
- The budget pauses it, visibly.
- `fray keepalive --stop` stops it within seconds; `leave` and the drive's
  bounds stop it; `fray team` reflects each.
- Closing the terminal does not stop it (a real terminal).
