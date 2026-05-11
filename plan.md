# Agent Orchestrator Implementation Plan

# Milestone 1 — GitHub App Foundation

Goal:
- receive webhook events
- parse `/agent`
- acknowledge command

Deliverables:
- GitHub App
- webhook endpoint
- signature verification
- command parser
- status comment
- GitHub Check Run support

Example:
```text
/agent ping
```

# Milestone 2 — Kubernetes Execution

Goal:
- launch Kubernetes Job per command

Deliverables:
- Kubernetes Job launcher
- in-memory concurrency control
- isolated execution container
- structured logs

Execution flow:
```text
webhook
  → orchestrator
  → k8s job
```

# Milestone 3 — GitHub Runtime Integration

Goal:
- clone repo
- checkout PR branch
- read PR context

Deliverables:
- GitHub installation token handling
- checkout logic
- PR metadata retrieval

# Milestone 4 — Local Model Integration

Goal:
- support local Qwen inference

Deliverables:
- OpenAI-compatible model adapter
- local vLLM endpoint support
- model routing abstraction

Example:
```text
/agent:reviewer:qwen-3.6 review this PR
```

# Milestone 5 — Devcontainer Runtime

Goal:
- support repo-defined runtime

Deliverables:
- detect `.devcontainer/devcontainer.json`
- launch devcontainer
- fallback runtime image

Execution:
```text
clone repo
  → devcontainer up
  → run agent inside runtime
```

# Milestone 6 — Commit-Capable Agents

Goal:
- allow coding agents to commit back to PRs

Deliverables:
- git commit support
- commit trailers
- push to PR branch
- audit metadata

Example trailers:
```text
Agent-Run: kiln_xxx
Requested-By: @user
Command: /agent:coder:qwen-3.6
```

# Milestone 7 — Recovery & Reconciliation

Goal:
- recover from orchestrator downtime

Deliverables:
- webhook redelivery
- scan recent comments
- stale run detection
- idempotency keys

Recovery logic:
```text
GitHub = source of truth
```

# Milestone 8 — Policy Engine

Goal:
- enforce permissions and runtime policies, using agent harness capabilities where agent/model validation is required

Rules:
- only maintainers can use cloud models
- reviewer agents cannot commit
- protected branches cannot be pushed to

# Milestone 9 — Git Provider Abstraction

Goal:
- prepare for GitLab support

Deliverables:
- provider interface
- GitHubProvider
- provider-neutral domain model

# Suggested Tech Stack

Language:
- Rust

Infrastructure:
- Kubernetes
- GitHub App
- Devcontainer CLI

Inference:
- vLLM
- OpenAI-compatible APIs

Container Runtime:
- Docker or Podman

# MVP Constraints

Avoid:
- databases
- Kafka
- Temporal
- workflow engines
- multi-agent orchestration
- memory systems

Focus on:
- simplicity
- auditability
- reproducibility
- GitHub-native workflows
