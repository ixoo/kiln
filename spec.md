# Agent Orchestrator Spec

## Goal

Build a simple Git-native agent orchestrator focused first on GitHub PR workflows, with a clean path to GitLab later.

The orchestrator reacts to comments like:

```text
/agent fix failing tests
/agent:reviewer review this PR
/agent:coder:qwen-3.6 fix this bug
/agent:designer:gpt-5.5-high propose a better UX
```

It launches an isolated Kubernetes Job, runs an agent runtime inside the repo-defined development environment, and reports progress back to the PR.

## Core Principles

- GitHub is the system of record.
- No database required for MVP.
- Orchestrator is stateless/recoverable.
- Kubernetes provides isolation and execution.
- Devcontainer defines the repo runtime.
- Agent profile defines behavior.
- Model selection is explicit and policy-controlled.
- Every action is auditable.
- Keep the orchestrator simple.

# Architecture

```text
GitHub App
  → webhook
  → orchestrator
  → command parser
  → policy resolver
  → Kubernetes Job
  → devcontainer runtime
  → agent runtime
  → model endpoint
  → commits/comments/checks back to GitHub
```

## Main Components

### Webhook Receiver

Responsibilities:
- verify GitHub webhook signature
- parse event
- detect `/agent` commands
- acknowledge quickly
- create/update GitHub status/check
- launch Kubernetes Job

### Command Parser

Supported grammar:

```text
/agent <task>
/agent:<agent> <task>
/agent:<agent>:<model> <task>
```

### Git Provider Abstraction

Provider-neutral abstractions:

- ChangeRequest
- AgentCommand
- RunStatus
- CodeComment
- ProviderUser
- ProviderToken

Initial implementation:
- GitHubProvider

Future:
- GitLabProvider

# Runtime Model

Runtime belongs to the repo, not the agent.

Resolution logic:

```text
if .devcontainer/devcontainer.json exists:
  use devcontainer
else:
  use default runtime image
```

# Agent Profiles

Agent profile controls live in the selected agent harness. Kiln treats agent names as opaque metadata until the harness integration can validate them.

Agent profile controls:
- prompts
- permissions
- behavior

# Model Routing

Model routing belongs to the selected agent harness configuration. Kiln preserves explicit model values from `/agent:<agent>:<model>` as opaque metadata and can later ask the harness which models are available.

Model aliases map to:
- local OpenAI-compatible endpoints
- cloud providers

Default:
- local model preferred
- cloud models explicit

# Kubernetes Execution

One command = one Kubernetes Job.

Properties:
- isolated
- ephemeral
- auditable
- disposable

# GitHub UX

Lifecycle:
- queued
- running
- analyzing
- editing
- testing
- pushing
- completed
- failed

# Auditability

Every run has:
- run ID
- requester
- command
- model
- agent profile
- commit SHAs

Commit trailers required.

# Recovery

GitHub is the source of truth.

Startup reconciliation:
1. redeliver failed webhook deliveries
2. scan recent comments for `/agent`
3. recover missing runs
4. mark stale runs

# Security

Rules:
- only maintainers can run commit-capable agents
- reviewer agents are comment-only
- cloud models require explicit permission
- installation tokens are short-lived

# Non Goals

Do not build:
- workflow engine
- Kafka
- Temporal
- multi-agent swarm system
- memory systems
- complex UI

Keep the orchestrator simple.
