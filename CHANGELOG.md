# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Changed

- Removed the placeholder `mcp-transport` and `forgejo-mcp` crates so the workspace now contains only `agentd`, `agentd-runner`, and `agentd-scheduler`, and added coverage that enforces that three-crate contract.
- Removed the vendored methodology skill distribution layer from the repository, including loadout configuration, manifests, sync and verify scripts, vendored skill copies, and related smoke tests.
- Replaced the old skill-focused GitHub Actions workflow with a Rust workspace CI workflow that runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace`, and `cargo test --workspace`.
- Updated repository documentation to describe methodology skills as externally provided rather than vendored in `agentd`.
