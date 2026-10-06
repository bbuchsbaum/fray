# Fray architecture

The maintained architecture and acceptance contract is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

Wakeable includes a declared managed-runner listener. Mote request cards stay
addressed to an actor who later leaves, and their local annotations never
replace Mote's authoritative open/answered state. `FRAY_STUCK_GRACE_MS` is
read by the daemon for its stuck scan and by the caller for the no-daemon
`fray stuck` fallback.

See [docs/REVIEW.md](docs/REVIEW.md) for the source review and product direction.
The original source archive remains in `fray-v0.1.0-source.zip`.
