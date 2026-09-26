# Existing-chat Codex wake adapter (experimental)

`wake.py` connects Fray attention to **one existing, loaded Codex App Server
thread**. It steers an active turn using its expected ID and starts a turn when
idle. Fray remains responsible for routing/receipts; Codex remains responsible
for execution and permissions. No new manager thread, terminal keystrokes, daemon
restart, automatic acknowledgment, model override, or approval response is used.

This is optional Python integration code, not a new Fray core dependency. Python
3.11+ and a local Codex App Server exposing a WebSocket Unix socket are required.
The initial development target is Codex 0.157.1 on macOS; other versions and hosts
must be qualified before relying on unattended wakeups. The protocol's
experimental API is enabled for paginated turn inspection. The Codex host must
remain running, with the target thread loaded and its approval UI available.

## Configure and run

Use the agent/session already joined by the target chat. Never borrow another
agent's identity or use `join --takeover` to make this bridge work. Stop any other
attention consumer for this same session first. The adapter does not join or start
Fray. User authorization to keep managing work includes authorization for the
model turns this adapter starts; the delivery and lifetime limits bound that work.

```sh
python3 -m venv /tmp/fray-codex-venv
/tmp/fray-codex-venv/bin/pip install -r integrations/codex-wake/requirements.txt

/tmp/fray-codex-venv/bin/python integrations/codex-wake/wake.py \
  --socket /absolute/path/to/app-server-control.sock \
  --thread EXISTING_THREAD_ID --cwd /absolute/path/to/project \
  --home /absolute/path/to/project/.git/fray \
  --agent YOUR_AGENT --session YOUR_EXISTING_FRAY_SESSION \
  --state /absolute/private/path/wake-state.json \
  --selection involved --lifetime 14400 --max-deliveries 100 --probe
```

First run with `--probe`: this verifies the exact thread ID/workspace and exits
without subscribing, starting Fray, or submitting input. Remove `--probe` to run.
Use `--selection all` only for a steward intentionally following the broader
inbox. Keep the **same state path** across restarts. Run one adapter per target
thread; the state lock rejects duplicate processes using that state, but cannot
detect a second adapter intentionally given another state path.

The process logs its PID, target, watcher PID, deliveries, and stop reason as JSON
lines. A supervisor may run it in the background with stdout/stderr redirected to
a private log. Retain that PID and send SIGTERM to **that process only** to stop it.
It terminates/reaps its own Fray watcher without interrupting the Codex turn,
leaving Fray, or closing another client's connection. Maximum lifetime is 24 hours.
There is no auto-install, login service, or unbounded automatic restart.

## Delivery and recovery contract

- `watch --attention --reconnect` supplies complete exact receipts. Only batch
  references reach the chat; peer titles/bodies are not copied into instructions.
  Fray batches receipts, and the adapter coalesces host event bursts for 200 ms.
- Deduplication uses `(store_id, agent, id, through_seq)`, not the transient batch
  ID. Newer versions remain eligible. A changed store or target fails closed.
- Delivery acceptance is recorded durably and **never acknowledges Fray**. The
  manager reads the batch and acknowledges only after considering its contents.
  Ignored delivered receipts do not repeatedly start costly turns. Inspect the
  journal and Fray inbox when progress stops.
- Approval/user-input waits defer delivery until a host event changes the state.
  The bridge never answers server approval/tool requests or overrides sandbox or
  approval settings. A `host_request_pending` log requires attention through the
  normal Codex host UI; unattended approval completion is not supported.
- Explicit rejected requests cause a fresh status read (at most three attempts).
  Connections can reconnect at most five times per process. A lost response or
  crash after the durable `sending` record is **uncertain**, not safely retryable.
- On uncertain delivery, stop and inspect the target chat for `[Fray wake ID]`
  using the journal's ID. After establishing acceptance, change that entry's
  status to `delivered`; only if non-delivery is established set it to `pending`.
  Preserve the journal as evidence. Do not delete it to bypass the guard.
  `clientUserMessageId` is a correlation marker, not assumed server idempotency.

## Verification

```sh
/tmp/fray-codex-venv/bin/python -m unittest discover -s integrations/codex-wake -v
```

Tests include a real Unix WebSocket mock, reconnection, approval deferral,
same-thread start/steer, exact-receipt deduplication after restart, lost-response
fail-stop, identity mismatches, and watcher cleanup. A live read-only probe does
not prove delivery. Qualify both an active `turn/steer` and an actual idle
`turn/start`, retaining delivery IDs, turn IDs, and the resumed model's receipt.

Protocol reference: [Codex App Server](https://learn.chatgpt.com/docs/app-server).
