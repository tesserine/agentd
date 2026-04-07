# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Added

- Added a documented static agent configuration format in `examples/agentd.toml` plus strict TOML parsing in the `agentd` crate for agent identity, base image, methodology mounts, credentials, and static runa command settings.
- Added a Podman-backed session lifecycle in `agentd-runner` that creates ephemeral containers, mounts methodology assets read-only, clones a fresh repository workspace, runs `runa`, injects caller-resolved credentials, supports optional timeouts, and force-removes the container on teardown.
- Added explicit `SessionInvocation.repo_token` support in `agentd-runner` so private HTTPS repository clones can authenticate with a clone-only bearer token without exposing the token in `podman create` arguments, git argv, or persistent git config.

### Changed

- Removed the placeholder `mcp-transport` and `forgejo-mcp` crates so the workspace now contains only `agentd`, `agentd-runner`, and `agentd-scheduler`, and added coverage that enforces that three-crate contract.
- Removed the vendored methodology skill distribution layer from the repository, including loadout configuration, manifests, sync and verify scripts, vendored skill copies, and related smoke tests.
- Replaced the old skill-focused GitHub Actions workflow with a Rust workspace CI workflow that runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace`, and `cargo test --workspace`.
- Updated repository documentation to describe methodology skills as externally provided rather than vendored in `agentd`.
- Tightened static agent config parsing so file-loaded `methodology_dir` values resolve from the config file's absolute directory and agent or credential names with leading or trailing whitespace are rejected.
- Narrowed `agentd-runner` repository checkout to `https://`, `http://`, and `git://` `repo_url` forms only, rejecting local paths, SSH-style URLs, and credential-bearing URLs up front while private HTTPS authentication now flows through the explicit `repo_token` invocation field instead of URL userinfo or generic runtime env injection.
- Restored acceptance of trailing-slash repository remotes such as `https://example.com/repo.git/` while keeping `agentd-runner` restricted to public `https://`, `http://`, and `git://` `repo_url` schemes.
- Updated `agentd-runner` environment injection so empty resolved values are passed as direct `NAME=` assignments while non-empty values continue through Podman-managed secrets, avoiding Podman's zero-byte secret rejection without exposing non-empty secrets in `podman create` arguments.
- Updated `agentd-runner` session startup to create a standard `/home/{username}` workspace, run `runa run` as an unprivileged unix user via `gosu`, require `useradd` and `gosu` in the base image contract, and reject configured agent names that are invalid or reserved unix usernames during config validation.
- Updated `agentd-runner` resource-allocation rollback so cleanup failures in `prepare_session_resources` are logged to process stderr with the same lifecycle cleanup formatter already used for other runner cleanup paths, while the original secret-creation error remains the returned failure.
