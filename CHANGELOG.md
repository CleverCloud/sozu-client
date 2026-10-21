# Changelog

## 0.7.0 - 2026-10-21

- Drive Sōzu command channels through Tokio's non-blocking `AsyncFd` readiness
  instead of executing blocking socket reads and writes on an async worker.
  Tokio runtimes using the client must enable their I/O driver.
- Apply one configurable absolute deadline to every `send` and `send_all`. It
  covers the pool queue, socket connection and retry, complete write, every
  `Processing` response, and terminal response. The default is 30 seconds and
  `Client::with_request_timeout` changes it per client.
- Return `Error::Timeout` when the deadline expires and classify it as
  non-recoverable because Sōzu may already have applied the request. A pooled
  connection is discarded after every timeout, cancellation, or incomplete
  exchange, preventing a late response from satisfying a later request.
- Apply the same transport and deadline to the `unpooled` client. The new
  `connect_with_timeout` also bounds its initial connection; an interrupted
  exchange consumes the ambiguous connection and later calls return
  `unpooled::Error::Unavailable`.
- Change `ConnectionManager::Connection` from the raw Sōzu `Channel` to the
  opaque `sozu_client::channel::Connection`. Code using `Client`,
  `Pool<ConnectionManager>`, and `Client::from(pool)` is unchanged; code that
  directly manipulated a dedicated pooled `Channel` must migrate to the
  client API. The channel setup errors `Blocking` and unpooled `Blocking` are
  replaced by `NonBlocking` / `Register` and `Prepare`, respectively, and
  `channel::Error::Load` now boxes its configuration error.

## 0.6.2 - 2026-10-18

- Require `sozu-command-lib` 2.3.2 to align the client's protocol types with
  Sōzu 2.3.2. This release changes no client code. Deploy the Sōzu 2.3.2
  runtime itself to obtain its reconciliation and durable-state fixes.
- Direct `send` preserves an explicit Sōzu `Failure` as `Error::Failure` with
  the full `Response`, for pooled and unpooled clients. `send_all` writes a
  `LoadState` batch, and the 2.3.2 runtime can skip a refused or invalid entry
  while `LoadState` still finishes successfully. In a batch, add each cluster
  before its backends; use `send` when you need a per-order `Failure`.
- `sozu-command-lib` 2.3.2 changes public types this client does not construct.
  The list below is a non-exhaustive selection of migration-relevant public
  additions; see the Sōzu 2.3.2 changelog for the complete API changes.
  Struct literals must initialize the new fields (use `..Default::default()`
  where available): `ConfigState.certificate_metadata`,
  `AddBackend.unix_path` / `response::Backend.unix_path`, and
  `RequestHttpFrontend.header_predicates` / `response::HttpFrontend.header_predicates`.
  Exhaustive matches must handle the new variants: `RequestType::CloseSessions`,
  `RequestType::AddUnixBackend`, the guarded header-predicate and `SilentClose`
  frontend variants, `RedirectPolicy::SilentClose` and
  `ContentType::ClosedSessions`. Build orders with the `RequestType`
  constructors (`add_backend`, `add_http_frontend`, `add_https_frontend` and
  the removal constructors) instead of hard-coding legacy variants.
- `sozu-command-lib` 2.3.2 also adds health-check probe-port and HTTP header
  fields (sozu-proxy/sozu#1899 and #1901). The client does not construct
  them; the runtime behavior belongs to Sōzu. The protobuf `HealthCheckConfig`
  gains public `port: Option<u32>` (additive field 9) and a `headers` list, and
  the file/TOML `FileHealthCheckConfig` gains public `port: Option<u32>` and
  `headers`. Exhaustive Rust struct literals must account for the new fields:
  protobuf `HealthCheckConfig` can use `..Default::default()`. Its
  `FileHealthCheckConfig.headers` is a list of `FileHealthCheckHeader { name,
  value_file }`: a header name and the path of the file holding its value, never
  the value itself. Prefer deserializing TOML to a struct literal.
- Health-check `port`: when unset, probes keep targeting the backend data
  port; when set, HTTP and TCP probes use that port on the backend IP. The
  valid range is 1..=65535, and `port` is incompatible with a backend
  `unix_path`, since it names a port on an IP address. Before enabling it,
  upgrade the Sōzu main process, every worker, and the Sōzu CLI: an older
  worker ignores additive field 9 and probes the data port, and an older CLI
  doing a read-modify-write cluster patch can drop the field.
- Health-check `headers` follow the Sōzu 2.3.2 contract defined in
  sozu-proxy/sozu#1901. The protobuf types and tags are available from the
  published `sozu-command-lib` 2.3.2; CLI and runtime behavior belong to Sōzu
  2.3.2, not to this client. `headers` apply
  to HTTP health checks only and keep their configured order. Names must be
  unique case-insensitively; the runtime refuses duplicates, more than 32
  entries, and header sets outside its 8 KiB / HPACK safety bounds.
  Credentials such as `Authorization` are supported, and every custom header
  value is protected by default and held in memory only: Sōzu never writes a
  clear value to saved state or to any file it produces, and a value is
  zeroized when its owned buffer is dropped (copies made by the allocator,
  kernel, sockets or swap are outside that guarantee). Persisted state,
  queries, diffs, JSON output, logs and `Debug` expose only each header name
  with a process-keyed, full HMAC-SHA256 fingerprint and its public per-key
  epoch, never a value, so a configuration read back from a query or saved
  state cannot resupply the values.
- The fingerprint key is ephemeral: fresh on a cold start, retained across a
  hot upgrade or handoff. Persisted metadata from another epoch is
  unverifiable and counts as missing until the values are explicitly injected
  again, and Sōzu sends no probe without them. The operator ingress
  in Sōzu is TOML `value_file` and CLI `--header-file NAME=PATH`, where `PATH=-`
  reads standard input at most once; no literal TOML value, argv value or raw
  environment value is promised. Once the values are injected, H1 and h2c
  probes send them exactly. The request types that carry clear values are an
  ingress mechanism, not a persisted or general raw API: do not serialize or
  store them, nor place them in a `send_all` batch file. This client contains
  no source change for headers and does not read TOML value files, orchestrate
  the protected protocol, run capability checks or provide a security engine:
  the caller or integrator owns protected request types and mixed-version
  preflight. Before using `headers`, upgrade the Sōzu main process, every
  worker, and the Sōzu CLI together. Under the Sōzu contract, the new CLI must
  verify worker capability before it reads or sends protected values, and a new
  main refuses protected activation before any mutation if a targeted worker
  lacks that capability. A Sōzu 2.3.1 main does not answer
  `QueryProtectedHealthCheckSupport`: it sends no response. This generic client
  has no built-in timeout for that exchange, so the caller must run the
  capability query over its own time-bounded channel. Only `Ok` carrying
  `ProtectedHealthCheckReply` counts as the typed capability acknowledgement.

## 0.6.1 - 2026-10-14

- Require `sozu-command-lib` 2.3.1 to align the client's protocol types with
  Sōzu 2.3.1. Deploy the Sōzu 2.3.1 runtime itself to obtain the fix that
  restores worker configuration state when a frontend order is refused.
- Verify that published Sōzu 2.3.1 accepts client requests and that refusal
  responses remain explicit `Error::Failure` values for pooled and unpooled
  clients.

## 0.6.0 - 2026-10-14

- Update `sozu-command-lib` to 2.3.0 so Sōzu 2.3 configuration and command
  fields are preserved when state is loaded and re-emitted.
- Update every direct dependency floor to its current stable release.
- Raise the minimum supported Rust version to 1.93.1, matching
  `sozu-command-lib` 2.3.0.
- Store the payloads of `Error::CreatePool`, `Error::GetConnection`,
  `Error::Failure` and `unpooled::Error::Failure` behind `Box` to keep returned
  `Result` values compact. Consumers that destructure these variants must
  dereference the boxed payload; error display and recoverability are unchanged.
- Document the mixed-version rollback boundary for cluster state containing
  Sōzu 2.3 fields.
