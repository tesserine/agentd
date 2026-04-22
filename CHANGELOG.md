# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
### Changed

- `agentd run` no longer reads `agentd.toml`: it now accepts `--socket-path <PATH>`, otherwise discovers the daemon socket through `$XDG_RUNTIME_DIR/agentd/agentd.sock`, `/tmp/agentd-$UID/agentd.sock` with ownership and `0700` checks, then `/run/agentd/agentd.sock`; profile lookup and default-repo resolution now happen daemon-side.
- `agentd run` now accepts one per-invocation work surface without profile edits: `--work-unit <id>`, `--request <text>`, or `--artifact-type <type> --artifact-file <path>`. Request text is synthesized into `.runa/workspace/request/operator-input.json`, while artifact-file input places validated JSON at `.runa/workspace/<type>/<file-stem>.json`.
- `agentd-runner` now declares its real platform contract at compile time: the crate targets Linux only, and downstream non-Linux builds now fail explicitly instead of compiling dead fallback code into a non-functional binary.
- Session outcomes now follow the shared `commons` exit-code convention across `agentd` and `agentd-runner`: outcomes carry semantic labels plus raw exit codes, daemon and CLI surfaces report labels such as `blocked` and `generic_failure`, `agentd run` exits successfully for normal terminal states (`success`, `blocked`, `nothing_ready`), and timeout remains an agentd-layer outcome outside the shared exit-code vocabulary.
- Additional bind mounts now reserve only runner-owned targets (`/agentd/methodology`, `/home/{profile}`, and `/home/{profile}/repo` plus descendants), allowing supported read-only and read-write mounts elsewhere under `$HOME` without runner setup mutating host-backed data.
- Profile-declared bind mounts now reject overlapping container targets within the same profile, so nested targets fail validation before startup instead of reaching the container setup script.
- Persistent audit records now default to `$XDG_STATE_HOME/tesserine/audit/<profile>/<session_id>/`, falling back to `$HOME/.local/state/tesserine/audit/<profile>/<session_id>/` for rootless installs, with `daemon.audit_root` available as an explicit override for root-owned system installs such as `/var/lib/tesserine/audit/`.
- Completed audit records now seal directories to `0555` and non-symlink entries to `0444`, skip symlinks while sealing, and update `agentd/session.json` through atomic temp-file replacement instead of truncate-and-write.
- `agentd_runner::SessionSpec` now requires an explicit `audit_root` field, making the audit-record destination part of the runner API instead of an implicit process-environment override.

### Fixed

- Session teardown now skips audit finalization and sealing when cleanup fails, leaving `agentd/session.json` intentionally incomplete instead of marking a session complete while its audit bind mount may still be live.
- Completed session outcomes now remain caller-visible when only audit finalization fails after teardown cleanup succeeds.
- Audit sealing now refuses multi-linked entries before rewriting metadata, preventing host file mode changes through hard-linked audit aliases.
- Allocation rollback failure now preserves the incomplete audit-record signal instead of finalizing `agentd/session.json` after leaked cleanup state.
- Manual request-text input now rejects methodologies that do not declare canonical request support or that advertise an unsupported canonical request version, instead of synthesizing unchecked workspace content.

## [0.1.0] — 2026-04-10

First release. agentd is a daemon that runs autonomous AI agent sessions in
ephemeral Podman containers, enforcing isolation, credential hygiene, and
methodology governance.

### Daemon

- Foreground daemon with single-instance enforcement via PID file.
- Two-signal shutdown: first SIGTERM/SIGINT drains in-flight sessions,
  second force-exits.
- Startup reconciliation removes stale containers and orphaned secrets from
  prior runs, scoped per daemon instance.
- Structured JSON logging via `tracing`, with `AGENTD_LOG_FORMAT=json|pretty`
  format selection and `RUST_LOG`/`AGENTD_LOG` filter control.

### Operator interface

- Unix socket API for session dispatch.
- `agentd run <profile>` for manual single-session execution.
- Optional `repo` argument overrides the profile's configured default.

### Profiles

- Static TOML configuration: base image, methodology directory, credentials,
  and session command per profile.
- Profile names validated as safe unix usernames (used for in-container
  unprivileged execution via `gosu`).
- Optional profile-level `repo` default and cron `schedule` for automated
  dispatch.
- `methodology_dir` paths resolve relative to the config file's directory.

### Session lifecycle

- Ephemeral Podman containers: created per session, force-removed on
  teardown regardless of outcome.
- Methodology directory mounted read-only into the container.
- Fresh repository clone into the container workspace. HTTPS-only URL
  validation; SSH and local paths rejected.
- Unprivileged execution: session command runs as a non-root user via
  `gosu`, with the profile name as the unix username.
- Optional per-session timeout with forced teardown on expiry.

### Credentials

- Credential injection via Podman-managed secrets for non-empty values;
  direct environment assignment for empty values.
- Optional `repo_token_source` for private HTTPS clone authentication
  without exposing tokens in process arguments or git config.
- Credential source names resolve against daemon-process environment
  variables at dispatch time.

### Scheduling

- Cron-based profile scheduling evaluated in daemon-local time.
- Scheduled sessions dispatch through the daemon's Unix socket, sharing the
  same execution path as manual `agentd run` invocations.
