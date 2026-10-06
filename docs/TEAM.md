# The Team card

A project's team is described once, by the owner, in a pinned owner card
titled `Team`. Agents read it when they join (`fray team` prints it with the
roster and the gaps), and only the owner can change it. To revise it, decide
"Team" again: `fray team` shows the newest.

```sh
fray owner decide "Team" --summary "$(cat team.md)"
```

A template for `team.md`; edit the counts and rules to suit the project:

```
Roles wanted:
- steward: 1. Interactive Claude. Plans, routes work, lands approved changes,
  receives escalations. Writes little code.
- worker: 4. Owns Mote beads along seams; Codex and Claude mixed.
- reviewer: any idle worker. A review must come from the other host than
  the author (Codex reviews Claude, Claude reviews Codex).
Seams: each module boundary gets a contract card (fray-seam skill); its
  producer and consumer come from different hosts.
Landing: two keys. An approve from the other host at the exact SHA, then
  the steward merges.
Waking: Codex runs under `fray drive -- codex exec -`; interactive Claude
  arms with `fray arm` through its Monitor.
Needs the owner: pushes, releases, anything touching another repository.
```

For keepalive, the newest active **pinned owner** Team card may also set:

```text
keepalive model: MODEL_ID
keepalive budget: 2000000
```

These values are captured at start, so later Team edits do not silently change
a running companion's model or daily token limit. Duplicate keys, an empty or
invalid model, and a nonpositive budget refuse start. When omitted, the daemon
uses `FRAY_KEEPALIVE_MODEL` and `FRAY_KEEPALIVE_DAILY_TOKENS` (budget default:
2,000,000). If no model is configured, the host's recorded transcript model is
used; missing recorded model fails explicitly. Fixed host argv always includes
the selected model. Keepalive remains unannounced pending the live-host and
sandbox qualification described in [its design](design/keepalive.md).

Joining is then one sentence in a new terminal: "join the fray", or "join
the fray as a reviewer", "as a coder on bd-123", "as the steward", "as a
monitor". The `fray` skill's "Joining the team" section says how an agent
turns that into a role.
