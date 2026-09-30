# Working-tree evidence and versioned reviews

Fray can capture an explicitly selected working-tree scope and attach advisory
peer verdicts to its immutable identity. Mote continues to own candidate
acceptance, ownership and completion.

## Capture and verify

```sh
fray --json snapshot create --paths src tests Cargo.toml Cargo.lock
fray snapshot verify manifest:SHA256
```

Creation returns `manifest:SHA256`, the absolute bundle path, the base Git HEAD,
and file count. The default destination is
`BOARD_HOME/evidence/snapshots/SHA256/`; missing directories are created.
`--output DIRECTORY` chooses another destination and `--root REPOSITORY` selects
the Git working tree. With a custom destination, verify the returned bundle path.
No daemon or joined identity is required to capture or verify.

Paths are literal, repository-relative scopes, not globs. Directories include
tracked files and nonignored untracked files recursively. Capture includes the
current working bytes, executable bits, staged additions, and tracked deletions
as tombstones, including staged deletions. Staged bytes are not a separate
candidate. Ignored untracked files are excluded, and selecting an ignored
untracked file explicitly is an error. Empty directories are not represented.

The bundle contains `manifest.json` and `files/`. Its identity is the SHA-256 of
the exact versioned manifest bytes, which name the selected scopes, base HEAD,
file hashes, executable bits and deletion states. Absolute source paths and
capture time do not enter the identity. The base HEAD is provenance; HEAD alone
does not identify uncommitted work. Repeating an unchanged capture reuses the
verified bundle. A snapshot covers only its selected scopes.

Output must be outside every selected input scope. For an explicit whole-tree
capture, use `--paths . --output DIRECTORY_OUTSIDE_REPOSITORY`. Fray rejects
symlinks in selected source paths, Gitlinks/submodules, sparse or unmerged index
entries, special files, escaping paths and non-UTF-8 names. Git HEAD must exist.
Limits are 64 scopes, 16,384 files, 64 MiB per file and 256 MiB total. The command
uses local Git and `shasum -a 256` and never fetches or uploads content.

Capture hashes the copied bytes, then checks source bytes, metadata, deletion
states and Git inventory again before publishing the completed bundle. A detected
concurrent change fails capture and removes staging. This is best-effort detection,
not an atomic filesystem snapshot; coordinate with writers when exact coherence
matters. Private staging becomes the final bundle directory. Verification checks
manifest identity, inventory, bytes and executable bits; it detects later edits
but is not an adversarial filesystem-race sandbox.

Share the returned path and identity with the reviewer. On another machine,
transfer the whole bundle directory while retaining its SHA256 basename, then
verify it there. Fray does not transfer files. Put raw test logs and exit status
in an agreed shared evidence directory, and include their paths in the review.

## Request, revise and review

```sh
fray review request --to REVIEWER --title 'Parser fix' \
  --baseline manifest:BASE_SHA256 --candidate manifest:CANDIDATE_SHA256 \
  --ref mote:ISSUE --body-file request.md

fray thread ID --unread
fray review verdict ID object --at manifest:CANDIDATE_SHA256 --expect 1 \
  --body-file findings.md --ack-batch BATCH

fray review subject ID --expect 1 --at manifest:NEXT_SHA256
fray review verdict ID approve --at manifest:NEXT_SHA256 --expect 2 \
  --body-file verification.md
```

References accept `git:` plus a full lowercase 40- or 64-character commit hash,
or `manifest:` plus a 64-character SHA-256. Fray validates reference syntax;
the reviewer must separately check the commit or run `snapshot verify` on the
artifact. A declared reference is not proof that its contents exist or passed a
check. Mutable branch names, short hashes and `+dirty` are rejected.

The request freezes its baseline, title, summary and kind. A different review
scope needs a new request. Its author can move the candidate using the current
subject revision, shown as `sREV`; card edits use the separate `rREV`. Routing,
priority and ordinary conversation remain available. Candidate movement is
independent of a reviewer's conversation lease and invalidates previous verdicts,
even if a later candidate returns to an earlier hash. A stale revision or candidate
rejects a verdict without posting evidence, opening a child card or acknowledging
anything. The requester cannot supply its own peer verdict.

`approve`, `object` and `blocked` are advisory evidence. `object` creates a linked
objection using Fray's existing parent-closure gate; `blocked` creates a linked
question. Moving the candidate or approving it never closes those children.
Threads, inbox entries and attention packets show the current subject and stale
verdicts; the event history retains previous references. `--ack-batch` handles
only the review card's exact delivered version, atomically with the verdict.
No verdict closes a conversation, changes Mote, or grants landing authority.

## Compatibility

These daemon operations advertise `peer_discovery` and `review_subjects`.
Explicit use against an older daemon fails capability negotiation. Optional
peer hints quietly remain unavailable on older daemons. The new database schema
is version 3: opening a v1/v2 board upgrades it additively, preserving identity,
cards and receipts. Older daemons refuse a v3 database, so they cannot bypass
the immutable review-scope rule. Stop old daemons through the normal lifecycle
before replacing binaries; no live migration or installation is implied by
building this branch.
