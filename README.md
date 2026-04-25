# agentd

Autonomous AI agent runtime daemon. agentd runs agent sessions in ephemeral
Podman containers on infrastructure you control. Each session gets an isolated
execution environment — its own identity, credentials, a fresh repository
clone, and read-only methodology context — supervised from setup through
teardown. agentd prepares and supervises these environments; model inference and
MCP transport belong to the agent runtime inside the container.

The project targets Linux hosts. Non-Linux builds fail intentionally because
the runner depends on Linux runtime primitives including rootless Podman,
systemd user services, and SELinux-aware filesystem handling.

## Why

Running autonomous agents requires infrastructure: isolated environments,
credential injection, workspace setup, identity management. Operators building
this ad-hoc re-solve the same problems for each agent and each deployment.

agentd is the self-hosted runtime layer. The operator declares *what* through
agent configuration — which image, which credentials, which methodology.
agentd owns *how* — container lifecycle, privilege management, resource cleanup.
The agent gets an isolated, ephemeral workspace with exactly what it needs and
nothing more.

## Status

v0.1.1 — early development.

The session lifecycle works end-to-end: agent configuration, foreground daemon
startup, operator-triggered sessions, ephemeral Podman containers, credential
injection, execution, and teardown. Startup reconciliation cleans stale
resources from prior runs. Structured JSON tracing provides operational
visibility.
Agents may now declare a default repository and an optional cron schedule.
Manual runs still flow through `agentd run`, and scheduled runs dispatch
through the same daemon socket intake without introducing a separate job type.
Manual runs may also carry per-invocation work input without modifying the
agent: request text can be synthesized into a canonical request artifact, and
complete JSON artifacts can be placed directly into the session workspace when
the active methodology declares the relevant artifact type and schema.
Agents may also declare additional bind mounts for host-managed state such as
subscription auth directories. Independently of agent mounts, agentd now
persists each session's audit record under the rootless default
`$XDG_STATE_HOME/tesserine/audit/`, falling back to
`$HOME/.local/state/tesserine/audit/`, with `daemon.audit_root` available as an
explicit override for root-owned installs such as `/var/lib/tesserine/audit/`.

## Configuration

An agent is a named environment specification: base image, methodology
directory, optional additional bind mounts, optional default repo, optional
cron schedule, credentials, and exact command argv. Define agents in a TOML
config file — start from
[`examples/agentd.toml`](examples/agentd.toml):

```toml
# Static agent registry for agentd.
# An agent can carry its own default repo and optional schedule.

#[daemon]
# Optional explicit host path for persistent audit records. Rootless installs
# default to $XDG_STATE_HOME/tesserine/audit, falling back to
# $HOME/.local/state/tesserine/audit when XDG_STATE_HOME is unset.
# Root-owned system installs should typically point this at
# /var/lib/tesserine/audit.
#audit_root = "/var/lib/tesserine/audit"

[[agents]]
# Stable operator-facing agent name used for lookup and container identity.
name = "site-builder"
# Prebuilt image containing the agent runtime and runa.
base_image = "ghcr.io/example/site-builder:latest"
# Methodology directory to mount read-only into the session environment.
methodology_dir = "../groundwork"
# Default repository URL cloned for manual runs when `agentd run` omits a repo,
# and for every scheduled run of this agent.
repo = "https://github.com/pentaxis93/agentd.git"
# Optional five-field cron expression in daemon-local time.
schedule = "*/15 * * * *"
# Optional environment variable name resolved by the daemon for clone-only
# repository authentication. This value does not flow into the agent runtime.
repo_token_source = "SITE_BUILDER_REPO_TOKEN"
# Exact argv for the agent process. agentd handles runa init and runa run.
[agents.command]
argv = ["site-builder", "exec"]

#[[agents.mounts]]
# Additional host bind mounts are declared explicitly per agent.
# `source` must be an absolute host path and must already exist.
# `target` must be an absolute path inside the container and must not
# duplicate or overlap another mount target in the same agent.
# `read_only = true` is appropriate for host-managed auth directories.
#source = "/home/core/.claude"
#target = "/home/site-builder/.claude"
#read_only = true

[[agents.credentials]]
# Secret name exposed inside the session environment.
name = "GITHUB_TOKEN"
# Environment variable name read from the daemon's own process environment.
source = "AGENTD_GITHUB_TOKEN"

[[agents]]
# A home-repo review agent that carries its own review configuration and scans
# repositories beyond the repo used to launch the session.
name = "code-reviewer"
base_image = "ghcr.io/example/code-reviewer:latest"
methodology_dir = "../groundwork"
repo = "https://github.com/pentaxis93/agentd.git"
repo_token_source = "CODE_REVIEWER_REPO_TOKEN"
[agents.command]
argv = ["code-reviewer", "exec"]

[[agents.credentials]]
name = "GITHUB_TOKEN"
source = "AGENTD_GITHUB_TOKEN"
```

Credential `source` fields name environment variables in the daemon's process
environment — export them before starting the daemon. Additional `mounts`
entries are bind mounts: `source` must be an absolute existing host path,
`target` must be an absolute container path, targets must be unique within the
agent, and runner-managed targets are reserved: `/agentd/methodology`,
`/home/{agent}`, `/home/{agent}/.agentd`, and `/home/{agent}/repo` plus
their descendants. Other
targets under `/home/{agent}` remain supported, including read-only auth
mounts such as `/home/site-builder/.claude`. Additional mounts are not
relabelled; on SELinux-enabled hosts, operators must ensure each host path
already has a container-compatible label. The base image must provide
`/bin/sh`, `find`, `git`, `useradd`, `gosu`, `runa`, and whatever binaries the
declared agent command uses. When an agent declares `schedule`, it must
also declare `repo`. Schedules are evaluated in daemon-local time and missed
fires are not backfilled after downtime. Persistent audit records default to
`$XDG_STATE_HOME/tesserine/audit` or `$HOME/.local/state/tesserine/audit`; set
`daemon.audit_root` to override that for system installations.

## Running a Session

Build from source with `cargo build --release`. agentd targets Linux and
requires rootless Podman for container execution. Operational deployments also
assume systemd user services and the SELinux considerations described in
`ARCHITECTURE.md`.

Confirm the deployed binary with `agentd --version`.

Start the daemon:

```bash
agentd daemon --config /etc/agentd/agentd.toml
```

`agentd` with no subcommand is equivalent to `agentd daemon`.

The daemon runs in the foreground, reconciles stale resources from prior runs,
and binds a Unix socket for operator control. When `daemon.socket_path` and
`daemon.pid_file` are omitted, agentd chooses coordinated defaults from the
current runtime context:

- `$XDG_RUNTIME_DIR/agentd/agentd.sock` and `$XDG_RUNTIME_DIR/agentd/agentd.pid`
  when `XDG_RUNTIME_DIR` is set
- `/tmp/agentd-$UID/agentd.sock` and `/tmp/agentd-$UID/agentd.pid` for
  rootless environments without `XDG_RUNTIME_DIR`
- `/run/agentd/agentd.sock` and `/run/agentd/agentd.pid` for root-owned system
  installs

On SIGINT or SIGTERM, the daemon stops accepting connections and drains
in-flight sessions; a second signal exits immediately.
The Unix socket protocol is internal to `agentd` in `v0.1.x`: daemon and CLI must be the same build, and operators must restart the daemon after replacing the binary before using `agentd run` again.

Trigger a session through the running daemon:

```bash
agentd run site-builder --work-unit issue-42
```

Manual invocation supports exactly one intent surface at a time:

- `--work-unit <ID>` targets existing queued work
- `--request <TEXT>` synthesizes a canonical request artifact at
  `.runa/workspace/request/operator-input.json`
- `--artifact-type <TYPE> --artifact-file <PATH>` validates and places a
  complete JSON artifact at `.runa/workspace/<TYPE>/<file-stem>.json`

`--work-unit`, `--request`, and `--artifact-file` are mutually exclusive.

`agentd run` does not read `agentd.toml`. The client connects to the daemon by
either:

- explicit override with `--socket-path <PATH>`
- default discovery by runtime context:
  `$XDG_RUNTIME_DIR/agentd/agentd.sock` first when `XDG_RUNTIME_DIR` is set;
  for rootless XDG-unset clients, `/tmp/agentd-$UID/agentd.sock` before
  `/run/agentd/agentd.sock`; for root XDG-unset clients,
  `/run/agentd/agentd.sock` directly

Default discovery treats a candidate socket as available only when it answers
the agentd socket protocol `Ping` request with `Pong`; unrelated listeners,
silent sockets, malformed responses, and ambiguous probe errors fall through
to the next default candidate.

When the `/tmp/agentd-$UID/` fallback exists, the client requires that
directory to be user-owned and mode `0700`; otherwise it refuses with an
actionable error instead of trusting an insecure `/tmp` path.

Agent lookup and default-repo resolution now happen daemon-side. The client
may omit the positional repo argument when the named agent declares `repo`,
and an explicit repo still overrides the configured default:

```bash
agentd run --socket-path /custom/agentd.sock site-builder --work-unit issue-42
agentd run site-builder https://github.com/pentaxis93/agentd.git --work-unit issue-42
```

Text input is methodology-gated. `--request` is available only when the active
methodology declares artifact type `request`, ships `schemas/request.schema.json`,
and that schema advertises a supported canonical request version through
`x-tesserine-canonical.version`. In `agentd v0.1.x`, the supported set is
`1.0.0` only. Unsupported or undeclared request support is rejected before the
container is created.

Artifact-file input is generic. The CLI reads the file locally, requires UTF-8
JSON, derives the artifact id from the file stem, and sends structured JSON to
the daemon. The runner accepts that input only when the methodology declares
the artifact type in `manifest.toml` and ships a matching
`schemas/<type>.schema.json`.

Examples:

```bash
agentd run site-builder --request "Add a status page"
agentd run site-builder --artifact-type claim --artifact-file ./claim.json
agentd run site-builder https://github.com/pentaxis93/agentd.git --request "Review the last release candidate"
```

Both manual and scheduled dispatches use the same daemon socket intake. Inside
the container, the agent sees:

- An unprivileged user with `$HOME` at `/home/site-builder`
- A fresh clone of the repository at `/home/site-builder/repo`
- Repo-root `.runa` bridged to persistent audit storage
- Read-only methodology mount at `/agentd/methodology`
- A runner-managed read-only invocation-input mount at `/agentd/invocation-input` when manual input is supplied
- Any operator-declared additional bind mounts, read-only or read-write per agent
- Credentials injected as environment variables
- `AGENTD_WORK_UNIT` when the invocation includes one
- A pre-materialized artifact under `.runa/workspace/...` when the invocation includes `--request` or `--artifact-file`
- `runa init` state followed by `runa run --agent-command -- <argv>` from the repo directory

The container is force-removed on completion. The session's audit record
persists on the host under the resolved audit root
`<audit_root>/<agent>/<session_id>/`, with runa state in `runa/` and agentd
metadata in `agentd/session.json`. If teardown cleanup fails, or if audit
finalization attempts closeout and fails, that metadata remains intentionally
incomplete with no `end_timestamp` or `outcome`. On successful finalization,
agentd seals persisted runa entries read-only and publishes a read-only
`session.json` as the final commit point. Ancestor directories remain writable
so the final same-directory atomic replace can occur. The on-disk metadata does
not encode which incomplete path occurred; operators should use
`runner.lifecycle_failure` plus the surrounding `runner.session_outcome`,
`runner.session_error`, and `runner.session_teardown` events to disambiguate
cause.

## Scheduled Runs

Agents with `schedule` run autonomously while the daemon is up. The scheduler
evaluates cron expressions in daemon-local time and opens the same Unix-socket
client path that `agentd run` uses. Multiple scheduled agents may overlap,
and their sessions dispatch independently. Session outcomes do not affect later
schedule evaluation: the next occurrence runs at its next scheduled time.

## Going Deeper

- **[ARCHITECTURE.md](ARCHITECTURE.md)** — session lifecycle phases, container
  isolation model, credential flow, and workspace crate boundaries. How the
  system is built and why.
- **[AGENTS.md](AGENTS.md)** — development discipline, BDD workflow, commit and
  branch conventions. Read this before contributing.
- **[examples/agentd.toml](examples/agentd.toml)** — annotated agent
  configuration. Starting point for writing your own.

## License

[MIT](LICENSE)
