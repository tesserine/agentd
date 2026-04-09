# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Added

- Added a documented static agent configuration format in `examples/agentd.toml` plus strict TOML parsing in the `agentd` crate for agent identity, base image, methodology mounts, credentials, and static runa command settings.
- Added a Podman-backed session lifecycle in `agentd-runner` that creates ephemeral containers, mounts methodology assets read-only, clones a fresh repository workspace, runs `runa`, injects caller-resolved credentials, supports optional timeouts, and force-removes the container on teardown.
- Added explicit `SessionInvocation.repo_token` support in `agentd-runner` so private HTTPS repository clones can authenticate with a clone-only bearer token without exposing the token in `podman create` arguments, git argv, or persistent git config.
- Added extraction-ready tracing bootstrap in `agentd` plus structured runner lifecycle/session events, with timestamped JSON logs to stderr by default, an `info` default filter so normal session lifecycle records are visible without extra env setup, `runner.session_error` for pre-completion failures, stderr fallback for runner failure diagnostics when no tracing subscriber is installed, `AGENTD_LOG_FORMAT=json|pretty` for format selection, and `RUST_LOG`/`AGENTD_LOG` filter control.
- Added a real foreground `agentd` daemon with single-instance PID-file locking, a local Unix-socket operator interface, `agentd run` manual session dispatch, configurable daemon socket/PID paths, and optional per-agent `repo_token_source` resolution for clone-only HTTPS authentication.
- Added runner-owned startup reconciliation for daemon-managed Podman resources so daemon startup removes dead runner-owned `agentd-*` containers and orphaned runner-owned `agentd-*` secrets before binding the operator socket, and emits structured startup reconciliation events.

### Changed

- Clarified the credential source contract so examples, config doc comments, and architecture docs now describe `source` as a daemon-process environment variable name resolved with `std::env::var`, not an opaque secret-store reference.
- Renamed the `agentd` crate's shared dispatch-layer request and helper APIs from manual/operator-specific names to source-agnostic run names, including the socket-interface integration test surface, so scheduler and operator callers share one clearly neutral dispatch path.
- Removed the placeholder `mcp-transport` and `forgejo-mcp` crates so the workspace now contains only `agentd`, `agentd-runner`, and `agentd-scheduler`, and added coverage that enforces that three-crate contract.
- Removed the vendored methodology skill distribution layer from the repository, including loadout configuration, manifests, sync and verify scripts, vendored skill copies, and related smoke tests.
- Replaced the old skill-focused GitHub Actions workflow with a Rust workspace CI workflow that runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace`, and `cargo test --workspace`.
- Updated repository documentation to describe methodology skills as externally provided rather than vendored in `agentd`.
- Tightened static agent config parsing so file-loaded `methodology_dir` values resolve from the config file's absolute directory and agent or credential names with leading or trailing whitespace are rejected.
- Narrowed `agentd-runner` repository checkout to `https://`, `http://`, and `git://` `repo_url` forms only, rejecting local paths, SSH-style URLs, and credential-bearing URLs up front while private HTTPS authentication now flows through the explicit `repo_token` invocation field instead of URL userinfo or generic runtime env injection.
- Restored acceptance of trailing-slash repository remotes such as `https://example.com/repo.git/` while keeping `agentd-runner` restricted to public `https://`, `http://`, and `git://` `repo_url` schemes.
- Updated `agentd-runner` environment injection so empty resolved values are passed as direct `NAME=` assignments while non-empty values continue through Podman-managed secrets, avoiding Podman's zero-byte secret rejection without exposing non-empty secrets in `podman create` arguments.
- Updated `agentd-runner` session startup to create a standard `/home/{username}` workspace, run `runa run` as an unprivileged unix user via `gosu`, require `useradd` and `gosu` in the base image contract, and reject configured agent names that are invalid or reserved unix usernames during config validation.
- Replaced raw runner lifecycle stderr logging with structured `tracing` events carrying `container_name`, `session_id`, stage, lifecycle kind, and error fields, and added explicit session start, outcome, and teardown events around `run_session`.
- Scoped startup reconciliation ownership per daemon instance so only runner-created resources named `agentd-{daemon8}-{agent}-{session8}` and `agentd-{daemon8}-{session8}-{suffix}` are eligible for startup cleanup, while resources owned by other daemon instances or legacy pre-namespace names are ignored.
