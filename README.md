# Sōzu client

> This repository exposes a client to interact with [Sōzu](https://github.com/sozu-proxy/sozu).
> It allows to send one request at a time or batching them using the `LoadState` commands.

# Status

This client is under development, you can use it, but it may have bugs.

# Installation

To install this dependency, just add the following line to your Cargo.toml manifest.

```toml
sozu-client = "^0.6.2"
sozu-command-lib = "^2.3.2"
```

Version 0.6.2 requires `sozu-command-lib` 2.3.2, the Sōzu 2.3.2 protocol
types; it makes no promise about newer Sōzu versions.
It requires Rust 1.93.1 or newer. Keep `sozu-client` and `sozu-command-lib` on
compatible versions so request types are shared by both crates. Deploy the Sōzu
2.3.2 runtime itself to obtain its reconciliation and durable-state fixes.
Direct `send` returns an explicit Sōzu `Failure` as an error with the full
`Response`. `send_all` writes a `LoadState` batch, which the 2.3.2 runtime can
finish successfully while skipping a refused or invalid entry: add each cluster
before its backends, and use `send` when you need a per-order `Failure`.
This generic library neither reads TOML value files, orchestrates the protected
health-check protocol, checks worker capability, nor provides a security
engine: the caller owns protected request types and mixed-version preflight,
and the CLI and runtime behavior belongs to Sōzu. Before enabling the
health-check `port` or `headers` fields, upgrade the Sōzu main process, every
worker, and the Sōzu CLI. Header values stay outside ordinary config and state
types; a `FileHealthCheckHeader { name, value_file }` only references the file
holding one. See the changelog for the contract and the new public fields.

# Usage

You can use the client like this:

```Rust
let opts = ConnectionProperties::try_from(&PathBuf::from("path/to/sozu-configuration.toml"))
    .expect("the Sōzu configuration must be valid");
let client = Client::try_new(opts)
    .await
    .expect("the Sōzu command socket must be reachable");

client
    .send(RequestType::LoadState("path/to/state.json".to_owned()))
    .await
    .expect("Sōzu must accept the state file");
```

When rolling back a controller built with an older `sozu-command-lib`, avoid
rewriting clusters that use Sōzu 2.3 fields: an older `AddCluster` order
cannot preserve fields it does not know. Keep the 2.3-compatible client in
place until every controller that can emit cluster state has been upgraded.

See [CHANGELOG.md](CHANGELOG.md) for release notes.

# Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) files.

# License

See the [LICENSE](LICENSE) file.

# Getting in touch

- Twitter: [@FlorentinDUBOIS](https://twitter.com/FlorentinDUBOIS)
