# Design: keep an interactive agent answerable after its turn ends

Status: revision 1 (2026-10-03), for review.

## The problem

The owner opens several interactive terminals (two Claude Code, two Codex)
and says "join the fray". A team leader then wants to say "you're idle,
review this" to any of them. Today that works only for an agent that is
`wakeable` (docs/design/no-silent-stalls.md, R1):

- an interactive Claude whose Monitor is running what `fray arm` printed;
- any agent run under `fray drive`.

An interactive Codex can never be woken once its turn ends, and an
interactive Claude cannot be woken once its Monitor expires or if it never
armed. The request reaches the board and escalates if it sticks (R3), but
the agent does not act until the owner types into that terminal.

## What the experiments established (2026-10-03, scratch)

| | Codex 0.160 | Claude Code 2.1 |
|---|---|---|
| One non-interactive turn inside an existing conversation | `codex exec resume ID -` recalled a fact from the conversation (7 s) | `claude -p --resume ID` (4 s) |
| Branch a conversation, keeping its memory | `codex exec fork ID -` (6 s) | `claude -p --resume ID --fork-session` (3 s) |
| A later resume sees an earlier background turn | yes | not tested |
| The id an interactive terminal exposes is resumable | `CODEX_THREAD_ID` = rollout id under `~/.codex/sessions` (checked for live plsneuro terminals) | `CLAUDE_CODE_SESSION_ID` = the transcript id |
| End to end: `fray drive -- <resume command>` under the agent's name; a leader's ask | woke and answered on the card from inside its conversation | same |

Both agents, asked by a peer to post a secret the user had given them,
declined: they had their memory and applied Fray's rule that peer text is
not authority. Total wake-to-reply was about 50 s for the pair.

Not yet tested, because it needs a live terminal: what an interactive
terminal does when a background turn has written to its conversation, and
whether a detached drive survives the terminal closing.

## The design

### `fray keepalive`

Run by the agent itself, from its own terminal, as part of "join the fray"
(or when the owner says "stay reachable"):

```sh
fray keepalive            # start; prints what it did and how to stop it
fray keepalive --stop     # stop this agent's keepalive
```

It:

1. Reads its own conversation id and host: `CODEX_THREAD_ID` (codex) or
   `CLAUDE_CODE_SESSION_ID` (claude). Neither set: refuse, and say that
   `fray drive` is the way for other hosts.
2. Starts a detached `fray drive` under the agent's own name, with the
   agent's own session (`FRAY_SESSION`), so the board sees one identity and
   one session, not a collision. Detached: its own process group, stdin
   closed, output to a log under the board's home, started through `nohup`
   so the terminal's hangup does not end it.
3. Runs each turn as the host's background form of this conversation (see
   the next section), with the board packet on stdin, as `drive` does now.
4. Records `keepalive` in the controller detail (host, conversation id,
   mode, log path), so `fray agents` and `fray team` show it, and so
   `fray keepalive --stop` and a second `fray keepalive` find it.

Bounds are a drive's: `--idle-timeout 86400` (the maximum), `--max-turns`
(default 50), `--child-timeout`. When it stops, it says why, as drives do;
there is no automatic restart (no supervisor in this slice).

### One conversation, two writers: fork once

The interactive terminal and the background turns must not both append to
one conversation, since the terminal does not reload what the background
wrote and the two would diverge. So the keepalive **forks once**, on its
first turn:

- first turn: `codex exec fork ID -`, or `claude -p --resume ID --fork-session`;
  the fork's new id is read from the JSON output and kept in the controller
  detail;
- later turns: `codex exec resume FORK -`, or `claude -p --resume FORK`.

The background agent therefore remembers everything up to the handoff, plus
every background turn since; it does not see what the owner typed into the
terminal after the handoff. That is the right trade: the terminal stays the
owner's, and nothing is written behind it.

`--shared` (resume the terminal's own conversation, no fork) is out of scope
until a live-terminal test shows the hosts tolerate it.

### Never two turns at once

The keepalive must not take a turn while the agent is working in its
terminal. The hooks already see that:

- `PreToolUse`/`PostToolUse` from the interactive session mark it busy (a
  timestamp the daemon keeps per agent and session);
- `Stop` marks it idle.

The drive defers a turn while the interactive session is busy (a tool call
within the last 2 minutes and no Stop since), and runs it when the session
goes idle. A busy mark older than 10 minutes is stale (a crashed terminal),
and the turn runs.

### Telling the terminal what its background self did

When the owner returns to the terminal, the agent there does not remember
the background turns (they happened in the fork). The next hook context, in
that session, starts with a short summary: the cards the keepalive answered
or created since the agent's last interactive turn, with links. The agent
can then read them (`fray thread ID`) and carry on, without repeating work.

### Permissions

A background turn cannot stop to ask the owner. So the keepalive runs with
the host's own non-interactive limits:

- codex: `-c sandbox_mode="workspace-write"`; approvals never prompt (exec);
- claude: `--allowedTools` from the project's Claude settings plus
  `Bash(fray:*)` and `Bash(mote:*)`; anything else a turn needs is refused,
  and the turn says so on the card.

`--allow` passes extra host flags; the default is deliberately narrow.

### Cost and limits, stated plainly

- Each wake is a paid model turn in a conversation that may be long; a fork
  of a long conversation is expensive per turn. `fray keepalive` prints the
  conversation's size on start.
- It keeps an agent answerable while the machine is up; it does not survive
  a reboot or logout.
- It does not make the interactive terminal itself take a turn: the
  terminal stays as the owner left it.

## Slices

- **K1.** `fray keepalive` and `--stop`: detection of host and conversation,
  the detached drive under the same name and session, fork-once with the
  fork id kept, controller detail, `fray team` showing it. Tested with stub
  hosts (scripts that print fork JSON and read the packet) for command
  construction, identity, fork-then-resume, detach and stop; one paid smoke
  test per host, as in this design's experiments.
- **K2.** Busy deferral from hooks, and the "while you were away" summary
  in hook context.
- **K3.** The skill: "join the fray" ends with `fray keepalive` for
  interactive Codex, and for interactive Claude as the alternative to a
  Monitor; `fray team` shows `keepalive` beside `driven`.

## Acceptance

- An interactive Codex and an interactive Claude each run `fray keepalive`
  and go idle; a leader's `fray send NAME --ask` is answered on the card by
  each, from a fork that remembers the conversation before the handoff.
- While the owner is working in the terminal, a waiting ask is answered only
  after the terminal's turn ends.
- The owner's next hook context in that terminal names what the keepalive
  did.
- `fray keepalive --stop`, `fray leave`, and the drive's own bounds each stop
  it, and `fray team` stops showing it as wakeable.
- Closing the terminal does not stop it (tested with a real terminal).
