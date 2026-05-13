# Agent Orchestrator Spec

## Goal

Kiln is a simple Git-native agent orchestrator focused first on GitHub pull request workflows, with a clean path to other providers later.

Kiln reacts to PR comments such as:

```text
/agent fix failing tests
/agent:reviewer review this PR
/agent:coder:qwen-3.6 fix this bug
```

Today, Kiln validates and acknowledges accepted commands, creates queued Check Runs, and can optionally launch a Kubernetes Job through `kubectl`. The agent harness, repository checkout, devcontainer execution, model inference, and commit push-back are future integrations and are not implemented in this repository.

## Core Principles

- GitHub is the system of record.
- No database is required for the MVP.
- The orchestrator is stateless and recoverable.
- GitHub API calls stay behind the `GitHubClient` trait.
- Kubernetes execution is optional and configured explicitly.
- Agent and model strings are opaque metadata until a harness validates them.
- Every accepted run has stable audit metadata.
- Keep the orchestrator simple.

## Implemented Architecture

```text
GitHub App
  -> webhook
  -> orchestrator
  -> command parser
  -> permission policy
  -> acknowledgement comment
  -> queued Check Run
  -> optional kubectl Job launch
```

## Target Architecture

```text
GitHub App
  -> webhook
  -> orchestrator
  -> Kubernetes Job
  -> agent harness
  -> repo checkout/runtime setup
  -> optional model endpoint
  -> comments/check updates/optional commits
```

The target architecture is aspirational. Add these parts only when explicitly requested and keep each step small.

## Main Components

### Webhook Receiver

Implemented responsibilities:
- Verify GitHub webhook signatures.
- Parse `issue_comment.created` events on pull requests.
- Detect line-start `/agent` commands.
- Reject malformed commands with a comment.
- Enforce maintainer-level invocation permission.
- Create acknowledgement comments and queued Check Runs.
- Optionally launch Kubernetes Jobs.

### Command Parser

Supported grammar:

```text
/agent <task>
/agent:<agent> <task>
/agent:<agent>:<model> <task>
```

Commands are recognized only at the start of a line. `/agentic` and `/agents` are ignored because they are not `/agent` commands.

### Policy

Implemented rule:
- The requester must have `write`, `maintain`, or `admin` repository permission.

Not implemented:
- Commit-capable agent authorization.
- Cloud model authorization.
- Reviewer/coder capability distinctions.

Those future rules require an agent harness contract. Kiln must not maintain its own model or agent catalog.

### GitHub UX

Implemented behavior:
- One acknowledgement comment per accepted command.
- One queued Check Run per accepted command.
- Deterministic run IDs in acknowledgement markers and Check Run `external_id` fields.
- Duplicate webhook deliveries for the same comment command are treated idempotently.

Domain foundations exist for future lifecycle states:
- queued
- running
- analyzing
- editing
- testing
- pushing
- completed
- failed

Only initial queued Check Runs are created today.

### Runtime Metadata

Runtime belongs to the repository and harness, not the orchestrator.

Implemented helper logic:

```text
if .devcontainer/devcontainer.json is a file:
  record devcontainer intent
else:
  record default runtime image
```

Kiln does not run Devcontainer CLI today.

### Kubernetes Execution

Implemented behavior:
- `disabled` mode acknowledges commands without launching jobs.
- `kubectl` mode renders a Kubernetes Job manifest and runs `kubectl apply -f -`.
- Jobs receive run, repository, PR, requester, command, agent, model, queue, and runtime metadata as environment variables.

Not implemented:
- Agent harness container behavior.
- Structured job log collection.
- Runtime lifecycle updates back to GitHub.

### Auditability

Every accepted run has:
- Run ID.
- Requester.
- Raw command.
- Optional agent string.
- Optional model string.
- PR head SHA in Check Run and job metadata.

Commit trailer helpers exist for future commit-capable agents. Kiln does not create commits today.

### Recovery

Implemented foundations:
- Deterministic run IDs.
- Hidden acknowledgement markers.
- Missing-run classification helper.
- Stale-run classification helper.

Not implemented:
- Startup reconciliation.
- Recent comment scanning.
- Webhook redelivery automation.
- Stale Check Run updates.

### Provider Abstraction

Implemented foundations:
- Provider-neutral domain types for change requests, statuses, comments, users, and tokens.

Not implemented:
- Generic provider trait.
- GitHub provider wrapper over that trait.
- GitLab support.

## Security

Implemented rules:
- Verify webhook signatures before parsing payloads.
- Require `write`, `maintain`, or `admin` permission to invoke agents.
- Use short-lived GitHub App installation tokens for GitHub API calls.
- Keep secrets out of repository configuration.

Future rules:
- Harness-declared capability checks for commit-capable agents.
- Branch protection and fork push safety for commit-capable agents.
- Harness-declared model policy.

## Non Goals

Do not build:
- Workflow engine.
- Kafka integration.
- Temporal integration.
- Multi-agent swarm system.
- Memory system.
- Complex UI.
- Kiln-owned model or agent catalog.

Keep the orchestrator simple.
