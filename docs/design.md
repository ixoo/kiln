# Kiln Design

Kiln is a small Git-native agent orchestrator focused on GitHub pull request workflows. GitHub is the system of record; Kiln avoids databases, external queues, workflow engines, and Kiln-owned model or agent catalogs.

## Current Scope

- `POST /webhooks/github` for GitHub webhook delivery.
- `POST /callbacks/agent` for authenticated agent completion callbacks.
- `GET /healthz` for health checks.
- GitHub webhook signature verification with `X-Hub-Signature-256`.
- `/agent` command parsing from line-start commands outside Markdown code fences.
- Opaque agent/model command metadata for the agent harness.
- Maintainer-level permission enforcement: `write`, `maintain`, or `admin`.
- Deterministic `kiln_<hash>` run IDs for idempotency.
- One Check Run per accepted command.
- GitHub-backed per-PR queue state using signed hidden run metadata in GitHub App-authored PR comments.
- In-memory per-PR critical sections to prevent same-process launch races.
- Optional local process execution mode with no isolation.
- Optional `kubectl` Kubernetes Job launch mode.
- Kubernetes agent completion callbacks that mark runs terminal and advance the next queued run.
- Trigger-based stale-run wakeups after non-terminal launches.
- Runtime detection helpers for `.devcontainer/devcontainer.json` versus fallback images.
- Audit metadata helpers for future commit trailers.
- Recovery helpers for missing/stale run classification.
- Provider-neutral domain types for future provider support.
- Local HTTP fixture tests with a mocked GitHub client.

## Architecture

```text
GitHub App
  -> webhook
  -> orchestrator
  -> command parser
  -> permission policy
  -> acknowledgement comment
  -> queued Check Run
  -> GitHub-backed queue scan
  -> optional local process launch
  -> optional kubectl Job launch
  -> authenticated completion callback
```

The target architecture can include a dedicated agent harness, repository checkout/runtime setup, optional model endpoints, comments, Check Run updates, and optional commits. Those pieces are intentionally outside this repository until explicitly implemented.

## Commands

Supported grammar:

```text
/agent <task>
/agent:<agent> <task>
/agent:<agent>:<model> <task>
```

Commands are recognized only at the start of a line and outside Markdown code fences. `/agentic` and `/agents` are ignored because they are not `/agent` commands.

Agent and model strings are preserved as opaque metadata. Kiln does not validate or route them.

## GitHub UX

- One acknowledgement comment per accepted command.
- One Check Run per accepted command.
- Deterministic run IDs in acknowledgement markers and Check Run `external_id` fields.
- Hidden run-state metadata in trusted PR comments for queued, running, completed, and failed states.
- Duplicate webhook deliveries for the same comment command are treated idempotently.
- Multiple commands on one PR preserve per-PR queue ordering.
- Check Runs are created as queued and updated to running, completed, or failed for implemented execution modes.

## Execution Modes

### Disabled

`disabled` acknowledges commands and creates Check Runs without launching runtime jobs. This is the default and the safest smoke-test mode.

### Local

`local` runs `local_command` on the same host as Kiln. It has no sandbox and inherits only the explicit `KILN_*` environment variables plus the command's configured environment behavior.

Use local mode only for development or trusted single-host deployments.

### Kubectl

`kubectl` renders a Kubernetes Job manifest and runs `kubectl apply -f -`. Kiln itself must run in an environment where `kubectl` can reach the target cluster.

Kubectl mode requires:

- `[execution].callback_url`
- `KILN_AGENT_CALLBACK_SECRET`

Jobs receive a per-run `KILN_CALLBACK_TOKEN` derived from Kiln's private callback key. The private callback key is never passed to agents.

## Agent Environment

Local commands and Kubernetes Jobs receive:

- `KILN_RUN_ID`
- `KILN_REPOSITORY_OWNER`
- `KILN_REPOSITORY_NAME`
- `KILN_REPOSITORY`
- `KILN_PR_NUMBER`
- `KILN_GITHUB_INSTALLATION_ID`
- `KILN_HEAD_SHA`
- `KILN_REQUESTER`
- `KILN_COMMAND`
- `KILN_TASK`
- `KILN_AGENT`, when specified
- `KILN_MODEL`, when specified
- `KILN_QUEUE_POSITION`
- `KILN_DEFAULT_RUNTIME_IMAGE`
- `KILN_CALLBACK_URL`, when configured
- `KILN_CALLBACK_TOKEN`, when callbacks are configured

## Completion Callbacks

Agents report completion with:

```http
POST /callbacks/agent
X-Kiln-Callback-Token: <KILN_CALLBACK_TOKEN>
Content-Type: application/json
```

```json
{
  "run_id": "kiln_...",
  "status": "completed",
  "owner": "octo",
  "repo": "repo",
  "repo_full_name": "octo/repo",
  "pr_number": 42,
  "installation_id": 999,
  "detail": "optional human-readable detail"
}
```

Use `"status": "failed"` to mark a run failed. A successful callback updates GitHub-backed run state and advances the next queued run for the PR.

## Configuration

The default TOML config acknowledges commands without launching runtime jobs:

```toml
[server]
bind_address = "127.0.0.1:3000"

[execution]
mode = "disabled"
namespace = "default"
job_image = "ghcr.io/ixoo/kiln-agent:latest"
default_runtime_image = "ghcr.io/devcontainers/base:ubuntu"
launch_timeout_seconds = 300
stale_run_seconds = 3600
# local_command = ["kiln-agent"]
# callback_url = "https://kiln.example.com/callbacks/agent"
```

The binary reads these environment variables:

- `KILN_ENV_FILE`: optional path to an env file to load before reading other settings.
- `KILN_CONFIG`: path to the TOML config file. Defaults to `config/kiln.toml`.
- `KILN_GITHUB_APP_ID`: numeric GitHub App ID.
- `KILN_GITHUB_WEBHOOK_SECRET`: webhook secret configured on the GitHub App.
- `KILN_STATE_SECRET`: private key used to sign GitHub-backed queue state markers.
- `KILN_PREVIOUS_STATE_SECRETS`: optional comma-separated previous state marker keys accepted during rotation.
- `KILN_AGENT_CALLBACK_SECRET`: private key used by Kiln to derive per-run callback tokens.
- `KILN_GITHUB_PRIVATE_KEY_PATH`: path to the GitHub App private key PEM.
- `RUST_LOG`: optional tracing filter.

Startup rejects empty, short, and example placeholder secret values. Generate webhook, state, and callback secrets with `openssl rand -hex 32` or an equivalent secret generator.

## GitHub App Requirements

Minimum tested repository permissions:

- Checks: read and write.
- Issues: read and write.
- Pull requests: read and write.
- Metadata: read-only.

Subscribe the app to the `Issue comment` event. GitHub emits main pull request conversation comments as `issue_comment` events because pull requests are issues in GitHub's data model.

## Recovery

Implemented foundations:

- Deterministic run IDs.
- Hidden acknowledgement markers.
- Missing-run classification helper.
- Stale-run classification helper.
- Trigger-based stale wakeup after a non-terminal launch.

Not implemented:

- Startup reconciliation.
- Recent comment scanning.
- Webhook redelivery automation.
- Reconciliation for stale runs that existed before the process started.

## Boundaries

- The default execution mode is disabled; local and Kubernetes execution require explicit config.
- Local execution has no sandbox and runs with the Kiln process user's host permissions.
- Kiln generates runtime job metadata, but the agent harness container is not implemented in this repository.
- Kiln detects devcontainer intent in helper code, but does not run Devcontainer CLI itself.
- Model routing and agent/model validation belong to the agent harness.
- Database-backed state is intentionally out of scope; GitHub remains the source of truth.
- Commit creation and push-back are not implemented.
