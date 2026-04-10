# agentd

Autonomous AI agent runtime daemon. agentd runs agent sessions in ephemeral
Podman containers on infrastructure you control. Each session gets an isolated
execution environment — its own identity, credentials, a fresh repository
clone, and read-only methodology context — supervised from setup through
teardown. agentd prepares and supervises these environments; model inference and
MCP transport belong to the agent runtime inside the container.

## Why

Running autonomous agents requires infrastructure: isolated environments,
credential injection, workspace setup, identity management. Operators building
this ad-hoc re-solve the same problems for each agent and each deployment.

agentd is the self-hosted runtime layer. The operator declares *what* through
profile configuration — which image, which credentials, which methodology.
agentd owns *how* — container lifecycle, privilege management, resource cleanup.
The agent gets an isolated, ephemeral workspace with exactly what it needs and
nothing more.

## Status

v0.1.0 — early development.

The session lifecycle works end-to-end: profile configuration, foreground daemon
startup, operator-triggered sessions, ephemeral Podman containers, credential
injection, execution, and teardown. Startup reconciliation cleans stale
resources from prior runs. Structured JSON tracing provides operational
visibility.

Scheduling policy does not exist yet — sessions are triggered manually via
`agentd run`.

## Configuration

A profile is a named environment specification: base image, methodology
directory, credentials, and runtime command. Define profiles in a TOML config
file — start from [`examples/agentd.toml`](examples/agentd.toml):

```toml
# Static profile registry for agentd.
# Session-specific inputs such as repo and work unit come from the CLI at run time.

[[profiles]]
# Stable operator-facing profile name used for lookup and container identity.
name = "codex"
# Prebuilt image containing the agent runtime and runa.
base_image = "ghcr.io/example/codex:latest"
# Methodology directory to mount read-only into the session environment.
methodology_dir = "../groundwork"
# Static session command executed from the cloned repository. This example
# keeps runa as the runtime, so the profile owns runa initialization, the
# agent command written into `.runa/config.toml`, and optional work-unit
# forwarding from the generic AGENTD_WORK_UNIT contract.
command = [
  "/bin/sh",
  "-lc",
  '''
runa init --methodology /agentd/methodology/manifest.toml
cat > .runa/config.toml <<'EOF'
[agent]
command = ["codex", "exec"]
EOF
if [ -n "${AGENTD_WORK_UNIT:-}" ]; then
  exec runa run --work-unit "${AGENTD_WORK_UNIT}"
fi
exec runa run
''',
]
# Optional environment variable name resolved by the daemon for clone-only
# repository authentication. This value does not flow into the agent runtime.
repo_token_source = "CODEX_REPO_TOKEN"

[[profiles.credentials]]
# Secret name exposed inside the session environment.
name = "GITHUB_TOKEN"
# Environment variable name read from the daemon's own process environment.
source = "AGENTD_GITHUB_TOKEN"
```

Credential `source` fields name environment variables in the daemon's process
environment — export them before starting the daemon. The base image must
provide `/bin/sh`, `git`, `useradd`, `gosu`, and whatever binaries the
configured session command uses.

## Running a Session

Build from source with `cargo build --release`. Requires rootless Podman for
container execution.

Start the daemon:

```bash
agentd daemon --config /etc/agentd/agentd.toml
```

`agentd` with no subcommand is equivalent to `agentd daemon`.

The daemon runs in the foreground, reconciles stale resources from prior runs,
and binds a Unix socket for operator control. Default paths:
`/run/agentd/agentd.sock` and `/run/agentd/agentd.pid`. On SIGINT or SIGTERM,
the daemon stops accepting connections and drains in-flight sessions; a second
signal exits immediately.

Trigger a session through the running daemon:

```bash
agentd run codex https://github.com/pentaxis93/agentd.git --work-unit issue-42
```

`agentd run` reads the same config file and connects to the socket path defined
there. This dispatches a session using the `codex` profile. Inside the
container, the agent sees:

- An unprivileged user with `$HOME` at `/home/codex`
- A fresh clone of the repository at `/home/codex/repo`
- Read-only methodology mount at `/agentd/methodology`
- Credentials injected as environment variables
- `AGENTD_WORK_UNIT` when the invocation includes one
- The configured session command executing from the repo directory

The container is force-removed on completion. No session state persists on the
host.

## Going Deeper

- **[ARCHITECTURE.md](ARCHITECTURE.md)** — session lifecycle phases, container
  isolation model, credential flow, and workspace crate boundaries. How the
  system is built and why.
- **[AGENTS.md](AGENTS.md)** — development discipline, BDD workflow, commit and
  branch conventions. Read this before contributing.
- **[examples/agentd.toml](examples/agentd.toml)** — annotated profile
  configuration. Starting point for writing your own.

## License

[MIT](LICENSE)
