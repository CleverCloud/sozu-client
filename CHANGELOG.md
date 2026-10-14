# Changelog

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
