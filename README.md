# Sōzu client

> This repository exposes a client to interact with [Sōzu](https://github.com/sozu-proxy/sozu).
> It allows to send one request at a time or batching them using the `LoadState` commands.

# Status

This client is under development, you can use it, but it may have bugs.

# Installation

To install this dependency, just add the following line to your Cargo.toml manifest.

```toml
sozu-client = "^0.6.1"
sozu-command-lib = "^2.3.1"
```

Version 0.6.1 targets Sōzu 2.3.1 or newer and requires Rust 1.93.1 or newer.
Keep `sozu-client` and `sozu-command-lib` on compatible versions so request
types are shared by both crates. Deploy the Sōzu 2.3.1 runtime itself to obtain
the worker configuration rollback when a frontend order is refused; the client
continues to return that refusal as an explicit error.

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
