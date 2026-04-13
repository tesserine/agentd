# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
