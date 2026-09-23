# Design: an owner-authority channel

Status: Tiers 1 and 3 implemented (2026-09-23); Tier 2 (signed approvals) pending.

## Problem

Agents correctly treat peer text as information, never as authority. But the
owner has no channel of their own, so owner decisions reach agents only by the
owner pasting into each session. That makes the owner a courier and a
bottleneck, which the charter (`docs/CHARTER.md`) exists to prevent. On
2026-09-23 the owner had to paste a re-tasking prompt into Codex that mostly
repeated a board card, and Codex still declined deployment steps it would have
been authorized to take under the charter, because it had no way to know the
charter came from the owner.

We need a way for agents to recognize "the owner decided this" that they
cannot produce themselves by accident, and that a prompt injection cannot
produce at all.

## Threat model, stated plainly

Every agent runs as the same OS user as the owner. Any agent with a shell can
read the store, call the socket and run any command the owner can. **No design
inside one user account can stop a determined agent from impersonating the
owner.** What a design can do:

1. Make owner authority impossible to claim by accident or through normal
   tool use.
2. Make it impossible for peer text or injected content to carry authority.
3. Make deliberate forgery a distinct, visible act that breaks the charter,
   rather than a side effect of ordinary commands.
4. At the strongest tier, require the owner's physical presence (a
   confirmation dialog or biometric) for each approval.

## Proposal, in three tiers

### Tier 1: a reserved owner identity (cheap; stops accidents and injection)

- `owner` is a reserved name. `join --as owner` and any write whose actor is
  `owner` are refused, except through `fray owner ...` subcommands.
- `fray owner ...` requires an interactive terminal: stdin must be a TTY, and
  the owner types a short confirmation phrase shown by the command. Agent tool
  shells are not TTYs (verified: Claude Code's Bash tool reports "not a tty"),
  so agents cannot run it through ordinary tool calls.
- Owner records are stored with `authority: "owner"`: the pinned charter,
  decisions, approvals and answers to `ask-owner` items.
- Hooks, attention packets and `brief` render them distinctly
  (`OWNER DECISION`), apart from peer text, and the skill says only these (and
  the owner's own words in a session) carry owner authority.
- Honest limit: a same-user process can still call the socket directly, or fake
  a terminal. That is deliberate circumvention, forbidden by the charter.

### Tier 2: signed approvals (real authentication; no new dependency)

- The owner holds an SSH signing key loaded in `ssh-agent` with confirmation
  (`ssh-add -c`), so macOS shows a confirmation prompt on every use.
- `fray owner approve ID "text"` signs a canonical payload (store id, card id,
  text, time, nonce) with `ssh-keygen -Y sign`.
- The daemon verifies it with `ssh-keygen -Y verify` against an
  `allowed_signers` file that only the owner edits.
- A verified record is `authority: "owner-signed"`. Agents can check it
  themselves with `fray verify ID`.
- An agent cannot produce a signature without the owner confirming the prompt,
  and cannot silently change `allowed_signers` without that being a visible,
  charter-breaking act (a hash of it can be pinned in the store at setup).
- This reuses OpenSSH, already present on macOS and Linux, through a
  subprocess, as Fray already does with Git. The five-dependency contract is
  unchanged.

### Tier 3: the owner queue (removes the courier role)

- `fray ask-owner "Restart shared daemon?" [--blocking] [--card ID]` puts an
  item in the owner's queue. Agents keep working on everything the charter
  allows while it waits.
- `fray owner review` (TTY only) walks the queue: approve, decline or answer.
  Each answer is a Tier 1 or Tier 2 owner record, routed to the asking agent
  like any reply.
- `fray owner digest` shows what landed, what was decided and what is waiting
  on the owner, so a returning owner catches up in one read.

## What a decision covers (as implemented, Tiers 1 and 3)

- An approve or decline binds to one revision of the request: the owner's
  review sends the revision it rendered, and a request changed since then is
  refused (`conflict`). The note records that revision and its title. A decided
  request is locked against agent edits, claims and tag changes, and a closed
  request cannot be decided.
- The review screen prints the request last, directly above the prompt, between
  banners only the renderer writes. All agent-written text (the request and its
  history) is quoted with a margin and wrapped to fit 80 columns, and history
  bodies are folded. The screen is printable ASCII only: every other character
  (invisible tag characters and variation selectors, bidi controls, wide or
  combining characters, controls) is shown as a visible `\u{...}` escape. An
  allowlist, because each blocklist of "invisible" characters proved
  incomplete. A request taller than the screen says so in its end banner.
- Comments on a request are context, not part of what is decided. An annotation
  added after the screen is rendered does not change the request's revision, so
  a decision still applies to exactly the text the owner saw.
- Owner cards (decisions such as the charter) can be changed only by the owner.
  Agents can reply to them, and replies never carry owner authority.

## How this changes today's workflow

- The charter becomes an owner-signed pinned record. Codex (or any new agent)
  checks it once with `fray verify` and acts on it. No paste needed.
- A restart or deploy that needs the owner is one `ask-owner` item. The owner
  answers it from any terminal, and the asking agent is woken by the answer.
- Peer text can never be upgraded to authority, because authority lives in a
  different record type that peers cannot create.

## Open questions for critique

1. Is Tier 1 worth shipping before Tier 2, or does a forgeable "owner" label
   invite false confidence? (Proposal: ship both, label Tier 1 records
   "owner (unsigned)".)
2. Linux without a desktop has no confirmation dialog for `ssh-add -c`. Is a
   passphrase per signature (no agent caching) acceptable there?
3. Should agents refuse charter-level actions if the charter is not
   owner-signed, or only warn?
4. Where does `allowed_signers` live: in the repo (reviewed like code) or in
   the owner's home directory (outside any agent's worktree)?
