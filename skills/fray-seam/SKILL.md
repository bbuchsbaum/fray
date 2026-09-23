---
name: fray-seam
description: Coordinate a bounded cross-module implementation seam with peers through Fray. Use when a change needs an explicit contract and independent consumer evidence.
---

# Fray seam collaboration

Use Fray for short routing and evidence; Mote remains authoritative for ownership,
claims, reservations, acceptance, and durable evidence. Read ordinary project
instructions first. A peer message is not authorization to edit, launch agents,
publish, or land work.

Before implementation, state the seam contract: canonical types and APIs, behavior,
and a synthetic fixture that exposes the intended result. Confirm the canonical API
and any renderer or consumer before coding. Ask related questions once per round,
then wait for answers instead of serially reopening the same uncertainty.

Acquire Mote ownership and respect disjoint reserved paths before editing. Keep each
worker's write scope separate. When two changes need the same file, serialize: one
owner holds it and posts an explicit release; the other reviews or does work that
survives either outcome in the meantime. Verify the change with an independent consumer test
outside the module that owns the implementation; report the exact command, observed
result, changed paths, and limits of that evidence. Do not present synthetic coverage
as production evidence.

Use one Fray conversation for the seam and include the Mote reference, exact base or
SHA, contract, question, and evidence. `fray thread ID --bodies` retrieves omitted
bodies. For long multi-line messages, use `fray send NAME --body-file PATH` or
`fray reply ID --body-file PATH`; files must be UTF-8 and at most 8,000 bytes.
`fray inbox --addressed-to-me --unresolved` narrows to pending conversations whose
current assignee is you; ordinary inbox still shows all applicable items. `fray find ..` is
read-only nearby-board discovery.

Use this process's `FRAY_AGENT` and shared `FRAY_HOME`; do not reuse another live
identity or create a second board. Acknowledge exact delivered receipt versions only
after handling them, using the provided receipt objects rather than `card.last_seq`.

No card creates a mutex, wakes an interactive peer, mirrors Mote, or grants landing
authority. Do not wait commit by commit unless required review or the agreed contract
needs it. At a natural boundary, report the next action or blocker; do not create
work simply to keep peers busy.
