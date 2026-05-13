# Agent Orchestrator Implementation Plan

This plan tracks implemented foundations separately from future runtime and agent-harness work. Kiln should remain a small, stateless GitHub-native orchestrator. GitHub is the source of truth; no database, queue, workflow engine, multi-agent orchestration, or Kiln-owned model/agent catalog is planned for the MVP.

## Current Project Status

Implemented now:
- GitHub App webhook receiver for `issue_comment.created` events on pull requests.
- HMAC signature verification for `X-Hub-Signature-256`.
- Line-start `/agent` command parsing.
- Opaque agent/model metadata preservation from `/agent:<agent>:<model>`.
- Maintainer-level invocation policy: requester must have `write`, `maintain`, or `admin` repository permission.
- Deterministic `kiln_<hash>` run IDs for idempotency.
- Acknowledgement comments with hidden run markers.
- Hidden run-state metadata in PR comments for GitHub-backed queue reconstruction.
- One queued GitHub Check Run per accepted command.
- Authenticated agent completion callback endpoint.
- In-memory per-PR critical sections only to prevent same-process launch races.
- Optional local process launcher with no isolation.
- Optional `kubectl apply -f -` Kubernetes Job launcher.
- Per-run callback tokens derived from Kiln's private callback key.
- Runtime detection helper for `.devcontainer/devcontainer.json` versus fallback image metadata.
- Audit trailer helpers for future commit-capable agents.
- Recovery classification helpers for missing/stale runs.
- Provider-neutral domain types as a foundation only.
- Local simulated HTTP tests with mocked GitHub API calls.

Not implemented in Kiln today:
- A complete agent harness.
- Repository clone/checkout inside a job.
- Devcontainer CLI execution.
- Model inference, model routing, or model authorization.
- Commit creation or push-back to PR branches.
- Lifecycle Check Run updates beyond the initial queued run.
- Startup reconciliation, webhook redelivery automation, or comment scanning.
- GitLab support.

## Milestone 1: GitHub App Foundation

Status: Done.

Delivered:
- GitHub webhook endpoint.
- Signature verification.
- `/agent` command parser.
- Permission checks through the GitHub API.
- Acknowledgement comments.
- GitHub Check Run creation.
- Local webhook fixture tests.

Quality bar:
- Keep GitHub API access behind the `GitHubClient` trait.
- Keep deterministic run ID generation stable.
- Preserve idempotency through hidden run markers and Check Run `external_id` metadata.

## Milestone 2: Kubernetes Execution

Status: Partial foundation.

Delivered:
- Local host process launch mode for simple trusted deployments.
- Optional `kubectl` launch mode.
- Disabled launch mode for local development and default deployments.
- Kubernetes Job manifest generation with run, repository, PR, command, requester, and runtime metadata environment variables.
- GitHub-backed queue state from hidden PR comment metadata.
- Agent completion callbacks that mark asynchronous runs completed or failed and advance queued work.
- Per-run callback tokens are passed to jobs instead of the private callback key.
- In-memory per-PR critical sections for race prevention, not durable queue state.

Remaining, when explicitly needed:
- Define the agent harness container contract.
- Decide how job logs are collected or linked from GitHub.
- Add tests around concrete Kubernetes manifest compatibility if the manifest evolves.

Out of scope for now:
- Building or publishing a Kiln-owned agent container image.
- Adding Kubernetes controllers, queues, or workflow engines.

Local execution warning:
- `local` mode runs with the Kiln process user's host permissions and provides no sandbox. It must be used only for development or trusted deployments.

## Milestone 3: GitHub Runtime Integration

Status: Partial foundation.

Delivered:
- GitHub App installation token handling.
- PR head SHA retrieval for Check Runs and job metadata.

Remaining, when the agent harness exists:
- Clone repository in the runtime job.
- Checkout the PR branch or head SHA.
- Provide PR metadata and context to the harness.
- Define failure reporting from the harness back to GitHub.

## Milestone 4: Agent Harness And Model Integration

Status: Future.

Kiln currently treats agent and model values as opaque metadata. It must not validate, route, or catalog agents/models until a harness integration defines that contract.

Future work, only when explicitly requested:
- Agent harness invocation contract.
- Model availability discovery through the harness.
- Local or cloud model routing inside the harness, not inside Kiln.
- Policy hooks based on harness-declared capabilities.

## Milestone 5: Devcontainer Runtime

Status: Future, with helper foundation only.

Delivered:
- Helper detection for `.devcontainer/devcontainer.json` as an actual file.
- Fallback runtime image metadata in config and job environment.

Remaining, when explicitly requested:
- Run Devcontainer CLI in the job environment.
- Define fallback behavior when a devcontainer exists but fails to start.
- Decide what runtime image or host dependencies are required by the harness.

Do not add Docker, Podman, or Devcontainer CLI execution to Kiln itself without an explicit request.

## Milestone 6: Commit-Capable Agents

Status: Future, with audit helper foundation only.

Delivered:
- Commit trailer rendering helpers.

Remaining, when explicitly requested:
- Commit creation in the agent harness.
- Push-back to PR branches.
- Branch protection and fork safety rules.
- GitHub UX for pushed commits and final status.

Example future trailers:

```text
Agent-Run: kiln_xxx
Requested-By: @user
Command: /agent:coder:qwen-3.6 fix tests
Agent: coder
Model: qwen-3.6
```

## Milestone 7: Recovery And Reconciliation

Status: Partial helper foundation.

Delivered:
- Missing-run classification helper.
- Stale-run classification helper.
- Idempotent run IDs and hidden comment markers.

Remaining, when needed:
- Startup reconciliation loop.
- Recent comment scanning.
- Webhook redelivery guidance or automation.
- Stale Check Run update behavior.

Constraint:
- Reconciliation must use GitHub as the source of truth and must not introduce a database.

## Milestone 8: Policy Engine

Status: Minimal implemented policy.

Delivered:
- Requester must have `write`, `maintain`, or `admin` permission.

Future policy hooks, only after a harness contract exists:
- Commit-capability checks declared by the harness.
- Model availability or model class restrictions declared by the harness.
- Protected branch and fork push safety rules for commit-capable agents.

Do not add Kiln-owned model aliases, agent catalogs, or hardcoded reviewer/coder behavior.

## Milestone 9: Git Provider Abstraction

Status: Foundation only.

Delivered:
- Provider-neutral domain types such as `ChangeRequest`, `RunStatus`, `CodeComment`, `ProviderUser`, and `ProviderToken`.

Future work, only when explicitly requested:
- A provider trait separate from `GitHubClient`.
- A concrete GitHub provider implementation over that trait.
- GitLab support.

## Current Tech Stack

Implemented:
- Rust.
- Axum.
- GitHub App APIs.
- Optional `kubectl` process invocation.

Possible future stack, not implemented today:
- Devcontainer CLI.
- Agent harness container.
- Local OpenAI-compatible model endpoints.
- Cloud model endpoints owned by the harness configuration.

## MVP Constraints

Avoid:
- Databases.
- Kafka.
- Temporal.
- Workflow engines.
- Multi-agent orchestration.
- Memory systems.
- Kiln-owned model or agent catalogs.

Focus on:
- Simplicity.
- Auditability.
- Reproducibility.
- GitHub-native workflows.
- Small explicit modules and mocked local tests.
