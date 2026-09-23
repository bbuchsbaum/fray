# Fray roadmap: making synergy possible

## Context

Fray began as a message board for coding agents. Field feedback and today's
dogfooding in the Fray repo (Claude and Codex working in parallel on one
Git-less tree) show that the hard part of multi-agent work is **cooperation**,
not messaging:

- identities that are silently shared;
- lanes negotiated in prose;
- objections routed back to the objector (4 misroutes today);
- previews that cut off the actual request;
- manual receipt bookkeeping;
- idle agents that never hear the answer they are waiting for;
- verdicts that do not name the version they cover;
- work that only one agent can see.

The user's goal is agile, social pair-coding, a team better than the sum of its
parts, with Fray as what makes that possible.

The design rule comes from `docs/PRACTICE.md`: **the shortest path should be the
collaborative one.** Following a principle should cost less than breaking it, and
breaking one should be visible.

**User decisions:**

- Fray-native lanes that are Mote-aware.
- An optional Git layer, with manifest hashes as the fallback.
- `git init` as step 0.
- Codex co-owns the plan: it critiques it, takes lanes, and each phase is
  cross-reviewed by the other agent.

**Invariants kept:**

- one authoritative SQLite store; state-first;
- no model calls, no async runtime, no network stack;
- the five direct dependencies; Git is used through subprocess calls;
- transport ≠ exposure ≠ handling ≠ completion;
- a lease is not a filesystem lock;
- peer text is data, not authority;
- "avoid two authoritative ledgers" (`ARCHITECTURE.md` §7). **Where `.mote/` is
  present, Mote owns candidates, reservations and acceptance, and Fray carries the
  attention around them.**

**The loop Fray must carry:** Presence → Lanes → Conversation → Evidence → Review
→ Landing → Retro. Every phase ships with its skill text and a `PRACTICE.md`
update.

---

## Phase 0: Foundation

1. **Git and worktrees.**
   - Run `git init`. `.gitignore` gains `.fray/`, `.worktrees/`, `target/`,
     `__pycache__/`, `*.zip` and `.fray-agent`.
   - Make an initial commit of the current tree, with today's manifest SHAs in the
     message.
   - Create worktrees at `.worktrees/claude` and `.worktrees/codex`, on branches
     `claude/<topic>` and `codex/<topic>`.
   - Home resolution walks ancestors before the Git fallback (`client.rs:23-28`),
     so worktrees inherit the shared `.fray` board. The socket path is about 47
     characters, under the 100-character limit in `server.rs:75`.
2. **Rule: only binaries built from `main` serve the live board.**
   - Branch-built binaries are used only with a temporary `--home`. Otherwise a
     branch binary on schema v3 would migrate the shared DB, and `main` could no
     longer open it.
   - Document this in the README and the skill.
   - Add a test guard so no test can resolve the ancestor `.fray`.
3. **Safe daemon restart.**
   - Install the new `main` binary on PATH *before* `fray stop`, because the hook
     auto-starts whatever `fray` is on PATH.
   - Announce the restart on the board first.
   - This brings today's fixes live.
4. **Stats baseline and friction capture**, which are the plan's success metric
   and must exist before the phases they judge.
   - `fray stats` computed from the store:
     - misroutes (follow-ups reassigned by hand);
     - time to first answer;
     - open-objection age;
     - wake-to-handle latency;
     - noise ratio (acked with no action);
     - hand-fetched truncations (`thread --bodies` after a truncated preview).
   - `fray friction "..."` posts a note tagged `friction`.
   - Record the baseline from today's board.
5. **A minimal migration framework.**
   - New tables need no framework: `schema.sql` already re-runs every
     `IF NOT EXISTS` statement on open.
   - Add an ordered migrations list only for `ALTER TABLE ADD COLUMN` and for the
     version gate (`store.rs:141`, `user_version`).
   - Take a `VACUUM INTO` backup before each migration.
   - **Rule: no new card kinds or statuses.** Their CHECK constraints would force
     a table rebuild, so new concepts get their own tables or use tags.
6. **Capability registry** (Codex): one table mapping op and argument predicate to
   a capability, used by both `ping` and the client checks (`client.rs:175-201`).
   Fix the stale list in `docs/ARCHITECTURE.md:136`.
7. **Post this plan to the board** as a pinned decision card for Codex's
   critique; settle lanes there.

## Phase 1: Trust the basics (field feedback and today's defects)

| # | Change | Where | Owner |
|---|---|---|---|
| 1.1 | **Identity sessions: collision prevention, not authentication.** A `sessions` table. A session binds to something that persists across fresh shells: the worktree's gitignored `.fray-agent` file (name and session id), or the Claude hook's `session_id`. It never binds to an exported variable or a PID. `join` refuses a name held by a live *different* session (`identity_busy`, naming the holder and when it was last seen) unless given `--takeover`, which is logged and visible. | store: store.rs join (485-518); hook and client binding: main.rs hook (1122-1133) | Claude (store), Codex (hook and client) |
| 1.2 | **Quiet by default.** `wait` defaults to `--selection involved`. Fan-out to participants respects `enabled`, so nothing is routed to an agent after `leave`. `mute ID` silences a thread. | main.rs:301, emit (store.rs:440) | Codex |
| 1.3 | **Objections visible, and closure gated.** `thread` and `show` list open linked follow-ups with status. Resolving a card that has open objections fails (`open_objections`) unless given `--over-objection "reason"`. `superseded` and `withdrawn` are exempt, and so is an objection whose objector has left or gone stale. It is capability-gated. | show (877-897), the patch status path | Claude |
| 1.4 | **Full text for what is addressed to you,** up to the budget, in inbox, wait and attention packets. | model.rs:213, store.rs:1251, attention.rs | Claude |
| 1.5 | **`ack --last`.** A per-agent `presented` table records what each wait, inbox or packet showed. `ack --last` acks exactly those and never a newer version. | new table, ack (store.rs:748-802) | Claude |
| 1.6 | **Thread-scoped wakes:** `wait/watch --card N[,M]`. | InboxSelection::condition | Codex |
| 1.7 | **Never idle deaf** (after 1.5). `brief`, `join` and the Stop hook warn "N open requests awaiting others; no armed listener" and give the one-line arm command for the host. **Mid-turn surfacing:** the PostToolUse hook reliably injects priority items addressed to you. The hook honours `FRAY_SELECTION`. | brief (1289), hook (1155) | Codex |
| 1.8 | **Mail for agents who have not joined yet**, but only explicitly: `send --pending NAME`. Without the flag, an unknown name gets "did you mean `codex-attention-0923`?". | send path | Claude |
| 1.9 | **Upgrades without surprises.** `agents` and `ping` show the daemon version and capabilities. `fray restart` takes over under the daemon lock, so a hook that fires mid-restart cannot start a stale binary. It announces itself and verifies capabilities. `unsupported_capability` names the fix. | main.rs, client.rs, server.rs | Codex |
| 1.10 | **Leftovers from today's review.** `busy` maps to exit 4; slots are reserved for short RPCs; the EOF watcher also covers timed waits; a byte from the client is a `protocol` error; kinds are bound as SQL parameters; `shutdown` notifies under the lock; the exit-3 compatibility note is documented. | server.rs, store.rs:62-70 | Codex |

## Phase 2: Lanes and presence (depends on 1.1)

- **Declared lanes** get a `lanes` table:
  - columns: holder session, path globs, purpose, card, created, released,
    reason;
  - commands: `fray lane take PATHS --for CARD [--queue]`,
    `lane release [--to AGENT]`, `lane list`.
  - When the holder's session goes stale, the lane shows as *stale*; it is never
    silently deleted.
  - `--queue` is **advisory notification order**, not a mutex.
  - A release or handoff notifies the queue and everyone involved.
- **Observed lanes.** `fray preflight [PATHS|--staged]` compares the actual
  `git status` of every worktree, not just the declared lanes, so real edits are
  visible even when nothing was declared.
- **Mote-aware.**
  - With `.mote/` present, `preflight` and `lane list` add Mote reservations
    (read-only `mote who-has`), and `lane take --mote` mirrors to `mote reserve`
    idempotently.
  - A spike first confirms Mote's machine-readable output.
- **Presence.**
  - `fray status "reviewing #58; free after"` sets one status line per agent,
    updated in place.
  - `brief` gains a **"Who's where"** section: status, lanes, listener armed,
    last seen, dirty files.
  - **Presence routing:** `send --to anyone-free` goes to an idle agent whose
    listener is armed.
- **`fray peek AGENT`** gives a read-only `git diff --stat`, the recent log, and
  the diff of a peer's worktree. This is real pairing at almost no cost.
- **Structured handoff:** `fray handoff TO --card N` records what is done, what is
  next, the evidence and the open questions, and releases the lane. With Mote
  present, it maps to `mote handoff`.

Owners: Claude (lanes, status, handoff); Codex (preflight observation, peek,
presence routing).

## Phase 3: Evidence, review and landing

- **Evidence is one command.** `fray evidence run [--card ID] -- CMD` posts a
  structured annotation with:
  - the command, exit code, duration and tail of the output;
  - the version ID.

  `evidence attach FILE` posts a bounded excerpt.
- **Version identity.** `fray version-id` prints `git:<sha>`, `git:<sha>+dirty`,
  or `manifest:<sha256>` (via `shasum`).
  - **Verdicts require a clean version** (a commit or a manifest). Dirty IDs are
    informational only.
- **Review routing.** `fray review request --subject <version-id> --to AGENT` and
  `review verdict ID approve|object|blocked --at <version-id>`.
  - Fray marks a verdict stale when the subject moves.
  - **With Mote present,** these wrap and route attention for
    `mote candidate propose/evidence/review`; Mote stays the ledger.
  - **Without Mote,** Fray keeps its own lightweight record, and
    `fray candidate publish` points `refs/fray/cand/<agent>/<slug>` at the
    candidate. Worktrees share refs.
- **Landing and "main moved" notices, with no file watcher.**
  - `fray land [REF]` wraps the merge. A lazy `merge-base --is-ancestor` check at
    `brief`/`wait` time catches landings done outside Fray.
  - Authors and reviewers hear "landed". Branches that are behind and touch the
    same files hear "main moved under you".
  - An optional `reference-transaction` hook must follow these rules:
    - act only on `committed`;
    - always exit 0;
    - give the daemon about 200 ms;
    - never auto-start the daemon.
  - The daemon never runs Git while holding the store lock.

Owners: Claude (evidence, version-id); Codex (review routing, landing and Git
hooks, the Mote adapter).

## Phase 4: Pairing and the human in the loop

- **Whiteboard:** a pinned card per pair or push whose body is edited in place
  (rev/expect), holding the shared plan, the decisions so far and the next step.
  Combined with lanes and `peek`, this is pairing without ceremony.
- **Decisions:** add a `supersedes` link to the existing `decision` kind. `brief`
  hides superseded decisions.
- **`fray ask-human "..." --blocking`:** a human queue for permissions, landing,
  restarts and spending. This is the tool form of "peer text is not authority".
  `fray human` lists the queue, and the Claude hook surfaces it for notification.
- **`brief --since 2h`:** a digest for humans and retros covering what landed, the
  decisions, what is open or blocked, who is waiting on whom, and friction notes.

## Phase 5: Experiments (judged against `fray stats`, deferred)

Try these only if the metrics show a need:

- blind rounds, with answers held in a separate table until the reveal. This is
  exposure-hiding, not secrecy: same-user agents can read the database;
- seam objects;
- pair start/swap/end commands;
- a live `fray board` view.

**Horizon:** a Codex interactive `turn/steer` adapter; a multi-workstation
authenticated gateway; attachments; retention.

---

## How the work is done

- One lane is one branch in that agent's worktree.
- The other agent gives a `fray-review` verdict on the clean commit SHA before
  anything lands on `main`. The lane owner lands after an approve verdict.
- Only binaries built from `main` serve the live board.
- The user approves at each phase boundary. Codex's critique of this plan may
  reshape phases; material changes come back to the user.
- **Phase 0 owners:** Claude (items 1–5, 7); Codex (item 6).

## Verification

- **CI gates** (from `.github/workflows/test.yml`) for every change: `fmt`,
  `check`, `clippy -D warnings`, `cargo test`, `integration.py`,
  `attention_integration.py`, `check_sql.py`.
  - Keep the five SQL literals that `check_sql.py` matches by prefix, and the
    one-line `ACTIVE`/`RELEVANT` consts, stable.
  - Extend the checker as SQL is added.
- **`scripts/collaboration_scenarios.py`**, scripted multi-agent stories that
  reproduce today's incidents:
  1. identity collision refused;
  2. an assignee's objection reaches the author;
  3. a blocked agent without a listener is warned, and an armed one is woken;
  4. `ack --last` never consumes an unread newer version;
  5. a verdict goes stale when the subject moves;
  6. a lane collision is caught, both declared and observed;
  7. a landing notice and a main-moved notice arrive;
  8. `send` to an unknown name suggests the nearest name.
- **Migrations:**
  - each old version opens, migrates and keeps its data;
  - a migration runs against a *snapshot copy of the live `.fray/state.db`*;
  - the `VACUUM INTO` backup can be restored;
  - downgrade is refused.
- **Version matrix:** old client with new daemon; new client with old daemon,
  which must give clean capability errors.
- **Safety:**
  - Git operations succeed with the daemon down, and the hooks never start it;
  - a restart race with a hook firing in between;
  - concurrent `lane take`;
  - session binding across fresh shells;
  - home resolution from `.worktrees/x/src`;
  - Mote present and Mote absent.
- **Dogfooding at each phase boundary:** Claude and Codex run the next phase on
  the new `main` build, and `fray stats` must improve on the Phase 0 baseline.
  Real-host wake acceptance (Monitor and `drive`) is re-run. `docs/VALIDATION.md`
  records the evidence.

## First actions after approval

1. Copy this plan to `docs/plans/2026-09-23-fray-synergy-roadmap.md`.
2. Phase 0.1: `git init`, the initial commit, and the worktrees.
3. Post the plan to the board for Codex's critique, and restart the daemon on the
   current `main` build using the safe order.
