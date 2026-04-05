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
- **An MCP transport layer**: runtimes such as Codex or Claude Code already speak MCP directly
- **An in-tree domain tool suite**: domain integrations live outside this workspace

The key architectural consequence is simple: agentd may configure tool availability for a runtime, but it does not proxy the MCP wire protocol or ship domain-specific MCP servers inside this repository.

## 2. Agent Capability Needs

Every structural decision in the workspace traces to a capability an agent session must eventually have.

### Network

**Need:** agents reach external services such as APIs, code forges, and web endpoints.

**Constraint:** the execution environment provides network access under deployment-specific policy.

### Credentials

**Need:** agents authenticate to those external services.

**Constraint:** credentials are injected at session setup and remain scoped to the owning agent.

### Identity

**Need:** agents know who they are and retain state across sessions.

**Constraint:** each agent receives a persistent home directory and stable identity variables.

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
| `agentd` | Composition root and daemon entrypoint. Parses configuration, assembles runner and scheduler components, and starts the process. | All, as orchestration | Keeps the binary thin and prevents subsystem concerns from collapsing into the entrypoint. |
| `agentd-runner` | Session lifecycle. Creates execution environments, injects identity, credentials, context, and tool configuration, launches runtimes, and tears sessions down. | Identity, Credentials, Mission, Tool Availability, Context, Network policy application | Session mechanics change independently from scheduling policy and should remain isolated. |
| `agentd-scheduler` | Triggering and timing. Determines when an agent session should start and with what mission context. | Mission | Scheduling policy has its own evolution path and should not be coupled to session setup mechanics. |

**Reading the table:** if the change is about when work starts, it belongs in `agentd-scheduler`. If it is about how a session is prepared, launched, or cleaned up, it belongs in `agentd-runner`. If it is about wiring the whole daemon together, it belongs in `agentd`.

## 4. Session Lifecycle

A session is one execution of one agent from trigger to teardown.

### Phase 1: Scheduling (`agentd-scheduler`)

The scheduler determines when an agent should run. Triggers may come from time-based schedules, external events, or manual invocation. The scheduler passes agent identity plus mission context to the runner.

### Phase 2: Session Setup (`agentd-runner`)

The runner prepares the execution environment:

1. Creates the container or equivalent isolated runtime environment.
2. Mounts the agent's persistent home directory and establishes identity variables.
3. Injects the credentials declared for that agent.
4. Mounts deployment-specific context read-only.
5. Makes required tools available, including any runtime configuration needed for direct MCP access to external servers.

### Phase 3: Execution (`agentd-runner`)

The runner launches the chosen agent runtime inside the prepared environment and supervises the session. Tool invocations happen directly from the runtime to installed CLIs or configured external MCP servers; agentd does not sit in the middle of that protocol exchange.

### Phase 4: Teardown (`agentd-runner`)

When the session ends or times out, the runner preserves agent state in the persistent home directory and cleans up transient execution resources.

## 5. Container Isolation Model

agentd is expected to run sessions in isolated environments such as Podman containers so agents remain separated from the host and from one another.

| Mount or Injection | Purpose | Need Served |
|---|---|---|
| Persistent home directory | Preserve identity, working files, and session-to-session state | Identity |
| Credentials | Authenticate to external systems without baking secrets into images | Credentials |
| Read-only context | Provide deployment facts without granting write access | Context |

From inside the environment, an agent should see:
- a stable `HOME`
- identity-related environment variables
- read-write access to its own persistent state
- the tools and runtime configuration needed for its assigned work

## 6. Credential Flow

Credentials are declared by agent configuration and sourced from an operator-managed secret store. During session setup, the runner resolves the declared credentials and injects them into the execution environment as environment variables or mounted secret files.

Isolation is per agent: one agent receives only its own declared credentials. Sharing access to the same external service still requires separate credential declarations per agent so compromise remains scoped.

## 7. Verification Matrix

| Need | Architectural Decision | Workspace Evidence | Failure if Violated |
|---|---|---|---|
| Network | Session environments receive deployment-controlled network access | `agentd-runner` owns session setup | Agents cannot reach external services |
| Credentials | Secrets are injected at launch, not stored in code or images | `agentd-runner` boundary and crate intent | Agents cannot authenticate or credentials leak across agents |
| Identity | Each agent has persistent state and stable identity variables | `agentd-runner` boundary and crate intent | Agents lose state across sessions |
| Mission | Scheduling hands mission context into session launch | `agentd-scheduler` plus `agentd-runner` boundary | Agents run without a reason or target |
| Tool Availability | Tools are provided through the environment; MCP remains a runtime concern | Three-crate workspace with no transport crate | agentd would absorb protocol work it does not need |
| Context | Deployment data is mounted read-only into sessions | `agentd-runner` boundary and crate intent | Agents operate without local awareness |
| Scheduling independence | Timing policy remains separate from execution setup | `agentd-scheduler` crate boundary | Scheduling changes would destabilize runner logic |
