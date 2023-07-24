# Sōzu client

> This repository exposes a client to interact with [Sōzu](https://github.com/sozu-proxy/sozu).
> It allows to send one request at a time or batching them using the `LoadState` commands.

# Status

This client is under development, you can use it, but it may have bugs.

# Installation

To install this dependency, just add the following line to your Cargo.toml manifest.

```toml
sozu-client = "^0.1.0"
```

# Usage

You can use the client like this:

```Rust
let opts = ConnectionProperties::try_from(&PathBuf::from("path/to/sozu-configuration.toml")).unwrap();
let client = Client::try_new(opts).await.unwrap();

client.send(RequestType::LoadState(...)).await.unwrap();
```

# Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) files.

# License

See the [LICENSE](LICENSE) file.

# Getting in touch

- Twitter: [@FlorentinDUBOIS](https://twitter.com/FlorentinDUBOIS)