# Daemon lifecycle: find, check and restart daemons in one step

## Context

On 2026-10-08 the owner asked for every Fray daemon on the machine to be moved
onto a freshly installed binary. That took ad-hoc work at every step:

1. **Discovery.** Fray has no list of its running daemons. `pgrep` found them,
   a truncated listing hid two (eyes4s, intaglio), and they were nearly left on
   builds from 2 and 3 October.
2. **Preflight.** The charter requires confirming "no one has a live wait"
   before a restart. Fray offers no check for that. The agent grepped
   `fray agents` JSON for `"live":true`, which misses connected watchers,
   keepalive drives and armed waits.
3. **Announcement.** `owner` is reserved for the owner's terminal, so the agent
   had to `join` as an invented identity (`claude-maint`), `post` and `leave`
   on each board, which leaves a stray participant behind. The first attempt
   failed, and the restart went ahead unannounced anyway, because nothing
   linked the two.
4. **Restart.** Stopping and starting are separate commands for each home.
   `shutdown` is abrupt: it drops every connection without telling clients that
   a restart is expected. Nothing checks that the new daemon runs the installed
   build. `start` launches `current_exe()`, so the result depends on which
   binary ran `start`.

The friction is in Fray itself, so the fix belongs in Fray (CHARTER: "report
friction in Fray itself as work to do").

## Goal

One command, `fray restart`, does the whole procedure safely for one home or for
every out-of-date daemon. `fray daemons` answers "what is running and is it
current?" without `ps`. The installer reports skew. Nothing restarts a busy
daemon without `--force`, and nothing restarts any daemon without being asked.

## Invariants

- One authoritative SQLite store per home; the daemon lock stays the single
  source of "who owns this home".
- No new dependencies and no unsafe code. Per-user state lives under
  `$FRAY_STATE_DIR`, defaulting to `$HOME/.local/state/fray`.
- The registry is advisory and only speeds discovery. Liveness is always
  confirmed by the lock or a ping, never by the registry file alone.
- Old daemons (protocol 2, without the new ops) remain restartable. The new CLI
  falls back to a degraded preflight and says it did so.
- No automatic restart of another session's daemon (REVIEW.md). Idle
  self-upgrade is parked behind an owner decision (L12).
- Transport ≠ handling: a "restarting" frame tells clients to reconnect. It
  never acknowledges anything.

## Work items (Mote epic `bd-01M4EHNRE1PYSNV9V34A7GE88M`, tag `daemon-lifecycle`)

| ID | Item | Depends on | Mote |
|----|------|------------|------|
| L1 | Daemon registry record: written after bind, removed on clean stop | none | `bd-01M4EHQCKZZPRMWBV2Z482AQGR` |
| L2 | `fray daemons`: list registered daemons with build skew and liveness | L1 | `bd-01M4EHQCQ539CNRZTCQX6JSM00` |
| L3 | Transitional discovery of daemons started before the registry existed | L2 | `bd-01M4EHQCTBQQ7XX0W555G1525N` |
| L4 | `occupancy` op: who would a restart interrupt | none | `bd-01M4EHQCXFDPZKGJQ40BV3CF58` |
| L5 | Restart preflight (`fray restart --dry-run`), with fallback for old daemons | L4 | `bd-01M4EHQD0MDSG7T8WDMRS4DVZH` |
| L6 | Daemon-authored maintenance notice (`announce` op, no join) | none | `bd-01M4EHQD3PEYA2TSEK6D7KAC51` |
| L7 | Graceful restart shutdown: a `restarting` frame, quiet client reconnect | none | `bd-01M4EHQD8HTFW1T170DCC97B0V` |
| L8 | Keepalive drives across a daemon restart | L7 | `bd-01M4EHQDCB1ACRKFYX64GC08W0` |
| L9 | `fray restart [--home H]`: preflight → announce → drain → start → verify | L5, L6, L7, L8 | `bd-01M4EHQDFFMH80WVMTRBBEFQ52` |
| L10 | `fray restart --all-stale` across the registry | L2, L9 | `bd-01M4EHQDJPE140EY9PRRCWVKSZ` |
| L11 | `install.sh` reports stale daemons and the restart command | L2, L9 | `bd-01M4EHQDPS4CD14F34NM6F32FN` |
| L12 | (Owner decision) idle self-upgrade onto a new installed binary | L9 | `bd-01M4EHQDVAKKZVDCQMZGH3Q1K4` |
| L13 | Docs, CHARTER line and shared skill updated; host skill copies synced | L9, L10, L11 | `bd-01M4EHQE0T7W35S45DB33KQAH2` |

Each work item's Mote bead holds its acceptance criteria. Every item needs
regression tests, `cargo fmt`, strict Clippy, `cargo test`, `integration.py`
where IPC behaviour changes, and independent review at the exact commit before
landing.

## Verification of the whole

Start daemons on three scratch homes with the old binary (`fray.previous`), plus
one with an attached `wait` and one with a keepalive drive. Install a new build
and confirm:

- `install.sh` lists all three as stale;
- `fray restart --all-stale` restarts the idle ones and refuses the busy ones,
  naming the holders;
- every restarted daemon pings the installed build, and its board shows one
  daemon-authored notice and no new participant;
- with `--force`, the waiting client reconnects and receives a message posted
  after the restart.
