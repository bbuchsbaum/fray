# Astra review correction evidence

This artifact retains the original OBJECT report and qualification for the
corrected Mote publication boundary and Fray cancellation/attention workflows.
The original report concerns the previous candidate SHAs; it is preserved as
historical evidence, not an objection to every successor.

`source-inputs.json` hashes the final Fray product/test/script inputs.
`provenance.json` records scope, counts and timing limits. The full Rust run
preceded test-only lint corrections; the changed admission tests passed again
in the required-check log. Exact successor peer verdicts are delivered on shared
Fray #109 and recorded on the Mote issue histories. This artifact does not itself
grant acceptance, installation or publication authority.

Raw logs are losslessly compressed as `.log.gz`; their uncompressed SHA256
values are recorded in `provenance.json`. Original status/timing sidecars are
retained separately. Compression preserves terminal blank lines and every
other original byte.

The real CLI court owns disposable Mote stores and Fray daemons; it uses no
models. Python host checks are synthetic transport fixtures. No native/paid
qualification, hosted CI or new soak is claimed. Detailed semantics and limits
are in [the revision 7 contract](../../design/mote-adapter.md).

`mote-b114f4b.patch.gz` losslessly preserves the unpublished Mote fix relative to
`a86803de31e890ac6e4331c9961705cabcde2fcb`. Extract it and apply it only in an
isolated Mote checkout at that base. Its raw SHA256 is in `provenance.json`.
Never activate a test binary against an original live store.
