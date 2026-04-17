# Architecture

## 1. What agentd Is

agentd is an autonomous AI agent runtime daemon. It runs agent sessions on infrastructure you control, decides when they run, prepares isolated execution environments, injects identity and credentials, and supervises execution from start to teardown.

agentd is:
- **Self-hosted**: operators run it on their own infrastructure
- **Runtime-oriented**: it prepares and supervises agent sessions rather than providing model inference
- **Modular**: scheduling and session execution evolve behind separate crate boundaries

agentd is not:
- **A hosted platform**: there is no control plane operated elsewhere
- **An AI model**: inference belongs to the chosen runtime or provider
- **An MCP transport layer**: MCP-capable runtimes already speak MCP directly
- **An in-tree domain tool suite**: domain integrations live outside this workspace

The key architectural consequence is simple: agentd may configure tool availability for a runtime, but it does not proxy the MCP wire protocol or ship domain-specific MCP servers inside this repository.

### Terminology

- **Profile**: a named, reusable environment specification in the daemon config — base image, methodology, optional default repo, optional schedule, credentials, and command. What the operator declares.
- **Session**: a single execution created from a profile plus invocation parameters (repo, work unit, timeout). What the runner manages.

## 2. Agent Capability Needs

Every structural decision in the workspace traces to a capability an agent session must eventually have.

### Network

**Need:** agents reach external services such as APIs, code forges, and web endpoints.

**Constraint:** the execution environment provides network access under deployment-specific policy.

### Credentials

**Need:** agents authenticate to those external services.

**Constraint:** credentials are injected at session setup and remain scoped to the owning profile.

### Identity

**Need:** agents know who they are and can be distinguished clearly per session.

**Constraint:** each session receives stable identity variables and a profile-derived container name inside an ephemeral runtime.

### Mission

**Need:** agents know why this session exists and what objective it serves.

**Constraint:** scheduling and invocation context flow into the session at launch time.

### Tool Availability

**Need:** agents can act on the world through installed tools and runtime integrations.

**Constraint:** CLI tools are present in the image or mounted environment, and any MCP-capable runtime receives configuration pointing at the external servers it should use. The runtime handles protocol communication directly.

### Context

**Need:** agents understand deployment-specific facts such as documentation, shared configuration, and local policy.

**Constraint:** context is mounted read-only into the execution environment.

## 3. Workspace Boundaries

The workspace contains three crates because there are three distinct rates of change and responsibility centers.

| Crate | Responsibility | Needs Served | Boundary Rationale |
|---|---|---|---|
| `agentd` | Composition root and daemon entrypoint. Parses configuration, owns the Unix socket intake, assembles runner and scheduler components, and starts the process. | All, as orchestration | Keeps the binary thin and prevents subsystem concerns from collapsing into the entrypoint while preserving one uniform dispatch path into session execution. |
| `agentd-runner` | Session lifecycle. Creates execution environments, injects identity, credentials, context, and tool configuration, launches runtimes, and tears sessions down. | Identity, Credentials, Mission, Tool Availability, Context, Network policy application | Session mechanics change independently of scheduling policy and should remain isolated. |
| `agentd-scheduler` | Triggering and timing. Determines when a session should start and with what mission context. | Mission | Scheduling policy has its own evolution path and should not be coupled to session setup mechanics. |

**Reading the table:** if the change is about when work starts, it belongs in `agentd-scheduler`. If it is about how a session is prepared, launched, or cleaned up, it belongs in `agentd-runner`. If it is about wiring the whole daemon together, it belongs in `agentd`.

## 4. Session Lifecycle

A session is one execution created from a profile, spanning trigger to teardown.

Before the daemon accepts any session trigger, it first reconciles stale
runner-managed Podman resources from prior runs of the same daemon instance.
Dead session containers named `agentd-{daemon8}-{profile}-{session16}` are
removed, then orphaned runner-managed secrets named
`agentd-{daemon8}-{session16}-{suffix}` whose session container no longer
exists are removed. The daemon instance id is derived from the configured
socket and PID paths, so distinct runtime-path pairs on the same host keep
separate ownership scopes. Only after that cleanup succeeds does the daemon
bind its Unix socket and begin accepting requests.

The daemon's Unix socket is the single intake for all session triggers. Manual
CLI invocation connects to that socket as a client. Scheduling policy also
connects to that socket as a client when it decides work should start. The
daemon accepts those run requests uniformly and dispatches them into session
execution. In `v0.1.x` this socket protocol is internal and unversioned:
daemon and CLI must be the same build, and replacing the binary requires a
daemon restart before new CLI invocations are supported.

Operational visibility for that lifecycle uses structured tracing events written
to stderr. The production default is timestamped JSON lines at `info` so
operators can monitor normal session start, outcome, teardown, and lifecycle
failures mechanically without extra log-filter setup; callers that invoke the
runner without installing tracing still retain direct stderr diagnostics for
failure paths. Local development can switch to a human-readable format through
environment configuration.

### Phase 1: Scheduling (`agentd-scheduler`)

The scheduler determines when a session should run. Today it evaluates each
profile's optional cron schedule in daemon-local time. When scheduling decides
to start a session, the scheduler is a socket client: it dispatches a run
request through the daemon's Unix socket, using the same intake path as manual
CLI invocation. The scheduler does not call the runner directly. Missed
occurrences while the daemon is down are not backfilled, and session outcomes
do not influence later schedule evaluation.

### Phase 2: Session Setup (`agentd-runner`)

The runner prepares the execution environment:

1. Creates an ephemeral Podman container from the profile's configured base image. That image must provide a POSIX-compatible shell at `/bin/sh` because the runner's container entrypoint executes through that path.
2. Sets identity inside the container, including `PROFILE_NAME` and a unique container name derived from the profile.
3. Injects caller-resolved credentials as environment variables for that session only via Podman-managed secrets rather than inline CLI arguments.
4. Mounts the configured methodology directory read-only.
5. Creates an unprivileged unix user whose username is the configured profile name, with home directory `/home/{username}`, and clones the requested repository into `/home/{username}/repo`. This clone step is a plain in-container `git clone`: the base image must provide `git`, `find`, `useradd`, and `gosu` in `PATH`, it accepts `https://`, `http://`, and `git://` repository URLs, rejects credential-bearing URLs up front, and can authenticate private HTTPS clones with an invocation-scoped bearer `repo_token`. The token is injected through a Podman secret, converted into one-shot git configuration for the clone process only, and removed before the session command starts. Base images that lack `/bin/sh`, `find`, `git`, `useradd`, or `gosu` are not supported.
6. Resolves the host audit root, creates it if needed, and probes writability before accepting work. The default for rootless deployments is `$XDG_STATE_HOME/tesserine/audit`, falling back to `$HOME/.local/state/tesserine/audit` when `XDG_STATE_HOME` is unset. Operators may override that with `daemon.audit_root`; root-owned system installs should typically point it at `/var/lib/tesserine/audit`. After resolution, the runner allocates a host audit record at `{audit_root}/{profile}/{session_id}/`, writes start metadata to `agentd/session.json`, and bind-mounts the `runa/` subtree into the container at `/home/{username}/.agentd/audit/runa` before the runtime initializes runa state.
7. Recursively transfers ownership of pre-existing content under `/home/{username}` while pruning host-backed bind-mount targets, the runner-owned audit leaf `/home/{username}/.agentd/audit/runa`, and `/home/{username}/repo`, then transfers ownership of `/home/{username}/repo` after the clone, sets `HOME=/home/{username}`, and keeps setup privileged only until the workspace is ready. The runner reserves `/home/{username}` itself, `/home/{username}/.agentd` plus its descendants, and `/home/{username}/repo` plus its descendants so host-backed bind mounts cannot collide with runner-managed paths.
8. Creates `/home/{username}/repo/.runa` as a symlink to `/home/{username}/.agentd/audit/runa`. This is a runner-owned repo contract: cloned repositories must not contain a `.runa` entry at repo root. If the clone already contains one, setup fails explicitly rather than overwriting repository content.

### Phase 3: Execution (`agentd-runner`)

The runner drops privileges with `gosu` and launches the profile's configured session command as the unprivileged session user from `/home/{username}/repo`, exporting `AGENTD_WORK_UNIT` when the invocation includes one, then supervises the container until natural completion or an optional timeout. Tool invocations happen directly from the runtime to installed CLIs or configured external MCP servers; agentd does not sit in the middle of that protocol exchange.

### Phase 4: Teardown (`agentd-runner`)

When the session ends or times out, the runner force-removes the container, finalizes `agentd/session.json` with end timestamp and outcome through an atomic same-directory temp-file rename, and seals the session record read-only on the host. The ephemeral container workspace still disappears, but the host audit record remains at `{audit_root}/{profile}/{session_id}/`.

If agentd is interrupted after writing start metadata but before finalization, the session record remains **incomplete**: `agentd/session.json` has `start_timestamp` but no `end_timestamp` or `outcome`. Operators should read that state as "the daemon stopped before closeout completed," not as a successful or failed terminal outcome.

## 5. Container Isolation Model

agentd runs sessions in ephemeral Podman containers so agents remain separated from the host and from one another.

| Mount or Injection | Purpose | Need Served |
|---|---|---|
| Read-only methodology directory | Expose the configured methodology manifest and protocol assets without allowing mutation | Context |
| Runner-owned audit bind mount at `/home/{username}/.agentd/audit/runa` | Persist runa state on the host while keeping agentd metadata distinct in the same session record | Context, Mission |
| Profile-declared bind mounts | Expose host-managed state such as subscription auth or persistent artifact storage with per-mount read-only vs read-write policy | Context, Credentials |
| Credentials | Authenticate to external systems without baking secrets into images | Credentials |
| Home workspace at `/home/{username}` with repo at `/home/{username}/repo` | Give the session a writable standard Linux home and a clean project workspace that starts fresh each run | Mission, Tool Availability, Identity |

From inside the environment, an agent should see:
- identity-related environment variables
- `$HOME` set to `/home/{username}`
- a read-only methodology mount rooted at `manifest.toml`
- a runner-managed audit bridge at `/home/{username}/repo/.runa -> /home/{username}/.agentd/audit/runa`
- any additional bind mounts declared by the selected profile
- a fresh writable repository checkout at `/home/{username}/repo`
- any runtime-managed state the configured session command creates inside the repo or home directory
- the tools and runtime configuration needed for its assigned work

Additional bind mounts are declared in profile configuration as `source`,
`target`, and `read_only`. agentd validates absolute container targets plus a
per-profile disjointness invariant: target paths must be unique and no target
may be a path-component prefix of another. It then stages canonical host
sources through runner-managed symlinks before calling Podman so host paths
containing commas remain mountable. Subscription auth is the first read-only
consumer of this mechanism; persistent audit storage in `#76` builds on the
same path with read-write mounts. Additional mounts are not relabelled; on
SELinux-enabled hosts, operators must pre-label those host paths with a
container-compatible context.

The internal audit mount is different from operator-declared mounts. It is
runner-owned, not operator-owned, and agentd applies `relabel=shared` to that
bind mount so the persisted `runa/` subtree remains writable on
SELinux-enforcing hosts such as Fedora CoreOS. `agentd/session.json` is not
mounted into the container; it stays host-only so runa-written state and
agentd-written metadata are distinguishable on disk without disambiguation.

Host audit records live under the resolved audit root, by default
`$XDG_STATE_HOME/tesserine/audit/<profile>/<session_id>/` or
`$HOME/.local/state/tesserine/audit/<profile>/<session_id>/` when
`XDG_STATE_HOME` is unset. Root-owned system installs should set
`daemon.audit_root = "/var/lib/tesserine/audit"`. Each record has this
layout:

- `runa/` — preserved runa state written naturally by the runtime
- `agentd/session.json` — agentd-written metadata (`session_id`, `profile`,
  `repo_url`, optional `work_unit`, timestamps, outcome, exit code when
  applicable) written by atomic temp-file replacement within the record
  directory

Coverage is intentionally scoped to the repo-root `.runa/` tree. That captures
`runa`'s non-configurable `.runa/store/` and the default `.runa/workspace/`.
If a methodology sets `artifacts_dir` outside `.runa/` in `.runa/config.toml`,
that workspace path is outside the audit mount and will not be preserved.
Groundwork uses the default `.runa/workspace/` layout and is fully covered.

Retention is intentionally out of scope here. Audit records accumulate
indefinitely under the resolved audit root; pruning and retention policy are
future work, so disk growth is currently an operator concern. Completed records
seal directories to `0555` and non-symlink entries to `0444`, so deleting old
records requires restoring write permission first, for example
`chmod -R u+w <record_dir> && rm -rf <record_dir>`.

The host security model is intentionally single-tenant. While a session is
running, agentd opens the mounted `runa/` subtree with mode `0o777` so writes
through the rootless container's UID mapping succeed. Any user with host shell
access can therefore read or write that subtree during the active session. On
completion, agentd seals directories to `0555` and non-symlink entries to
`0444`, making finished records world-readable on the host. For single-tenant
deployments such as babbie, that tradeoff is acceptable; a multi-tenant host
would need a different permission model before deployment.

The startup writability probe is intentionally local-filesystem scoped. It
verifies that the daemon can create and remove a file under the resolved audit
root before dispatch begins. That catches ordinary permission and path errors
early, but it does not validate network-filesystem behavior beyond the probe;
NFS and similar targets can still fail later with semantics the probe does not
model.

Session ids are 16 lowercase hex characters generated from `getrandom`, giving
roughly `2^64` possible values per profile and a birthday bound near `2^32`
sessions before collisions become materially likely. On collision,
`create_dir_all` would silently reuse the existing directory tree and merge two
records. That is not an operational concern at current scale, but operators
planning very long-lived or very high-volume deployments should understand the
risk envelope.

## 6. Credential Flow

Credentials are declared by profile configuration as daemon-side environment variable names. For each configured credential, the daemon resolves `source` with `std::env::var(source)` from its own process environment before calling `agentd-runner`. Operators provide those values through normal host mechanisms such as systemd `EnvironmentFile=`, shell exports, or container environment injection. During session setup, the runner receives only the already-resolved credential values, creates Podman-managed ephemeral secrets for non-empty values, and injects those values into the execution environment as environment variables without placing the secret values on the Podman command line. Empty assignments are injected directly as `NAME=` because Podman secrets reject zero-byte payloads. Once the container reaches the running state, the runner removes the backing Podman secret objects and relies on the in-container environment copy for the rest of the session.

Because startup reconciliation is scoped per daemon instance rather than to the
whole Podman namespace, the daemon removes only runner-managed resources whose
names carry its own derived daemon id: dead
`agentd-{daemon8}-{profile}-{session16}` containers are removed first, then
orphaned `agentd-{daemon8}-{session16}-{suffix}` secrets whose session
container is gone.

Repository clone authentication is a separate invocation concern rather than an agent runtime credential. When a profile declares `repo_token_source`, the daemon resolves that environment variable at dispatch time and, when the resolved value is non-empty, maps it to `SessionInvocation.repo_token`. The runner then injects that bearer token through its own ephemeral secret, uses it only for the `git clone` invocation, and unsets the internal token variable before the session command starts so the token does not persist in git config or the agent runtime environment.

Isolation is per profile: one profile receives only its own declared credentials. Sharing access to the same external service still requires separate credential declarations per profile so compromise remains scoped.

## 7. Verification Matrix

| Need | Architectural Decision | Workspace Evidence | Failure if Violated |
|---|---|---|---|
| Network | Session environments receive deployment-controlled network access | `agentd-runner` owns session setup | Agents cannot reach external services |
| Credentials | Secrets are injected at launch, not stored in code or images | `agentd` resolves configured environment-variable sources and `agentd-runner` accepts the resolved values | Sessions cannot authenticate or credentials leak across profiles |
| Identity | Each session receives stable in-container identity variables and container naming | `agentd-runner` session contract and Podman lifecycle | Operators cannot distinguish which profile a session belongs to |
| Mission | Scheduling or CLI invocation hands repo and optional work unit into session launch | `agentd-scheduler` plus `agentd-runner` boundary | Agents run without a reason or target |
| Tool Availability | Tools are provided through the environment; MCP remains a runtime concern | Three-crate workspace with no transport crate | agentd would absorb protocol work it does not need |
| Context | Methodology assets are mounted read-only into sessions and the repo is freshly cloned per run | `agentd-runner` boundary and crate intent | Agents operate without local awareness |
| Scheduling independence | Timing policy remains separate from execution setup | `agentd-scheduler` crate boundary | Scheduling changes would destabilize runner logic |
