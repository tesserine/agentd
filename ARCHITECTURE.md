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
5. Creates an unprivileged unix user whose username is the configured profile name, with home directory `/home/{username}`, and clones the requested repository into `/home/{username}/repo`. This clone step is a plain in-container `git clone`: the base image must provide `git`, `useradd`, and `gosu` in `PATH`, it accepts `https://`, `http://`, and `git://` repository URLs, rejects credential-bearing URLs up front, and can authenticate private HTTPS clones with an invocation-scoped bearer `repo_token`. The token is injected through a Podman secret, converted into one-shot git configuration for the clone process only, and removed before the session command starts. Base images that lack `/bin/sh`, `git`, `useradd`, or `gosu` are not supported.
6. Recursively transfers ownership of `/home/{username}` to that user, sets `HOME=/home/{username}`, and keeps setup privileged only until the workspace is ready.

### Phase 3: Execution (`agentd-runner`)

The runner drops privileges with `gosu` and launches the profile's configured session command as the unprivileged session user from `/home/{username}/repo`, exporting `AGENTD_WORK_UNIT` when the invocation includes one, then supervises the container until natural completion or an optional timeout. Tool invocations happen directly from the runtime to installed CLIs or configured external MCP servers; agentd does not sit in the middle of that protocol exchange.

### Phase 4: Teardown (`agentd-runner`)

When the session ends or times out, the runner force-removes the container and discards the entire ephemeral workspace. No session state is preserved on the host by the runner.

## 5. Container Isolation Model

agentd runs sessions in ephemeral Podman containers so agents remain separated from the host and from one another.

| Mount or Injection | Purpose | Need Served |
|---|---|---|
| Read-only methodology directory | Expose the configured methodology manifest and protocol assets without allowing mutation | Context |
| Credentials | Authenticate to external systems without baking secrets into images | Credentials |
| Home workspace at `/home/{username}` with repo at `/home/{username}/repo` | Give the session a writable standard Linux home and a clean project workspace that starts fresh each run | Mission, Tool Availability, Identity |

From inside the environment, an agent should see:
- identity-related environment variables
- `$HOME` set to `/home/{username}`
- a read-only methodology mount rooted at `manifest.toml`
- a fresh writable repository checkout at `/home/{username}/repo`
- any runtime-managed state the configured session command creates inside the repo or home directory
- the tools and runtime configuration needed for its assigned work

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
