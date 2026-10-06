# Offline retention

`fray prune --older-than WINDOW` is a read-only dry run. It selects terminal cards whose heads are older than the boundary and reports cards retained for live references, review subjects, Mote references (including historic event payloads after a display tag was cleared), unacknowledged receipts, or presented batches. It writes neither a lock nor archive output. Open-card references support `#ID`, `card:ID`, and `fray:ID`; linked-card `detail.parent_card` references are read as JSON fields, so formatting whitespace does not alter retention.

Use `fray prune --older-than WINDOW --archive DIRECTORY` only with the board offline. The command refuses the daemon lock, writes a complete Markdown archive and verified `state-before.sqlite`, then replaces selected event payloads with metric projections and an archive path. It retains event IDs, timestamps, card-head fields required by metrics, and separate retention audit records. The archive database can be restored with `fray restore` into a fresh home.

Compaction removes full event text from history search. It advances a durable cursor floor: watch, raw event, and inbox cursors before that floor fail explicitly and require a fresh snapshot. Statistics remain derived from the retained event projections; the archive holds conversation bodies.

No daemon or shared board is pruned by this command. Archive destinations must be fresh directories.
