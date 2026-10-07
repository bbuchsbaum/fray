# Local authority workflow evidence

`provenance.json` separates the corrected source `883b93a`, its precommit checks,
clean postcommit CLI build/courts, local Mote prerequisite `a86803de`, and historical
rejected source `7439784`. `peer-review.json` retains that objection and its
corrected-source approval. Final evidence-commit approval is recorded in the
local tracker and session, after this immutable package is committed.

`source-inputs.json` hashes every product/test/script input at the corrected
source. `manifest.json` hashes the retained files. Raw patch/test-log whitespace
is preserved losslessly in gzip so code whitespace checks remain meaningful.
For gzip entries, the manifest also records the uncompressed SHA256 and size.

To reconstruct the unpublished Mote prerequisite against upstream `63454fe`:

```sh
gzip -dc mote-a86803de-prerequisite.patch.gz > /tmp/mote-a86803de.patch
git apply --check /tmp/mote-a86803de.patch
git apply /tmp/mote-a86803de.patch
```

Apply only to an isolated checkout of the recorded Mote base; run its required
checks before using that build. Fray's real courts take explicit Fray/Mote
binary paths and create only disposable stores. Debug test failpoints drive
the crash/interruption fixtures. Original live stores were never activated,
no binary was globally installed, and no paid or native model trial was run.

Earlier passing logs at `7439784` retain their original scope; independent review
found three missing recovery/validation paths. They do not qualify completion.
The corrected source passed 452 Rust tests, 96 Python tests and 17 real CLI cases,
plus 10 synthetic public-CLI cases and CLI TTL parsing tests. The earlier soak
remains bound to its different source/binary and timing boundary.
