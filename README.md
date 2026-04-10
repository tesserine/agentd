# agentd

`agentd` is an autonomous AI agent runtime daemon. Run autonomous AI agents on infrastructure you control.

## Status

Early development. The current build supports:
- foreground single-instance daemon startup
- startup reconciliation of stale runner-managed Podman containers and secrets owned by the starting daemon instance before accepting new sessions
- local Unix-socket operator control
- manual `agentd run <profile> <repo> [--work-unit <wu>]` session triggers
- clone-only repository auth through optional `repo_token_source`

## Architecture Overview

The system is organized as a Rust workspace with focused crates for composition, session lifecycle, and scheduling. Agent runtimes handle MCP directly when they need it; `agentd` is responsible for preparing and supervising the execution environment. See [ARCHITECTURE.md](ARCHITECTURE.md) for the architecture document.

## Quick Start

1. Start from [examples/agentd.toml](examples/agentd.toml) and define at least one profile.
2. Export any runtime credential env vars named by `[[profiles.credentials]].source`.
3. Optionally export the env var named by `repo_token_source` when private HTTPS clones need a bearer token for `git clone`.
4. Start the daemon:

```bash
agentd daemon --config /etc/agentd/agentd.toml
```

`agentd` with no subcommand is the same as `agentd daemon`.

Before the daemon binds its Unix socket, it reconciles stale runner-managed
session containers named `agentd-{daemon8}-{profile}-{session16}` and orphaned
runner-managed secrets named `agentd-{daemon8}-{session16}-{suffix}` left
behind by prior runs of the same daemon instance. The daemon instance id is
derived from the configured socket and PID paths, so different runtime-path
pairs on the same host do not clean up each other's resources. Startup aborts
if that cleanup cannot complete.

The daemon is a foreground process. By default it uses:
- socket: `/run/agentd/agentd.sock`
- pid file: `/run/agentd/agentd.pid`

Override those paths in the config file:

```toml
[daemon]
socket_path = "/run/agentd/agentd.sock"
pid_file = "/run/agentd/agentd.pid"
```

Relative `socket_path` and `pid_file` values are resolved from the directory
that contains the config file.

On `SIGINT` or `SIGTERM`, the first signal stops accepting new operator
connections and drains in-flight sessions. A second signal exits immediately.

Trigger a manual session through the running daemon:

```bash
agentd run codex https://github.com/pentaxis93/agentd.git --work-unit issue-52
```

`agentd run` reads the same config file for daemon runtime paths. It ignores
the `profiles` registry, but the top-level config shape must still be valid, so
typos like `[deamon]` fail instead of silently falling back to default daemon
paths.

`repo_token_source` is not a runtime credential. It is resolved by the daemon at dispatch time, mapped to `SessionInvocation.repo_token`, and used only for the runner-managed `git clone`.

## License

Licensed under the terms in [LICENSE](LICENSE).
