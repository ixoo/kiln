# Kiln

Kiln is a Git-native agent orchestrator. It accepts `/agent` commands on pull request issue comments, validates policy, acknowledges accepted work with a PR comment plus a Check Run, and can hand accepted runs to a local process or Kubernetes Job launcher.

## Current Scope

- `POST /webhooks/github` for GitHub webhook delivery.
- `POST /callbacks/agent` for authenticated agent completion callbacks.
- `GET /healthz` for health checks.
- GitHub webhook signature verification with `X-Hub-Signature-256`.
- `/agent` command parsing from line-start commands only.
- Opaque agent/model command metadata for the agent harness.
- Maintainer-level permission enforcement: `write`, `maintain`, or `admin`.
- Deterministic `kiln_<hash>` run IDs for idempotency.
- One Check Run per accepted command.
- GitHub-backed per-PR queue state using signed hidden run metadata in GitHub App-authored PR comments.
- In-memory per-PR critical sections to prevent same-process launch races.
- Optional local process execution mode with no isolation.
- Optional `kubectl` Kubernetes Job launch mode.
- Kubernetes agent completion callbacks that mark runs terminal and advance the next queued run.
- Runtime detection helpers for `.devcontainer/devcontainer.json` versus fallback images.
- Audit metadata helpers for future commit trailers.
- Recovery helpers for missing/stale run classification.
- Provider-neutral domain types for future provider support.
- Local HTTP fixture tests with a mocked GitHub client.

## Commands

Supported forms:

```text
/agent <task>
/agent:<agent> <task>
/agent:<agent>:<model> <task>
```

Bare `/agent <task>` leaves agent and model selection to the agent harness. Explicit agent/model values are preserved as opaque metadata, but Kiln does not validate them locally.

## Local Development

Local development does not require Docker, Kubernetes, a devcontainer, or a webhook tunnel for the primary workflow. Tests simulate signed GitHub webhook requests through the real Axum route and mock outbound GitHub API calls.

Run tests:

```sh
cargo test
```

Run locally:

```sh
cp config/kiln.example.toml config/kiln.toml
cp .env.example .env
cargo run
```

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

Set `[execution].mode` to `local` for the simplest execution path. Local mode runs `local_command` on the same host as Kiln, without isolation, and passes run metadata through `KILN_*` environment variables. This is useful for development or trusted single-host deployments, but it is not the recommended isolation boundary.

Set `[execution].mode` to `kubectl` only in an environment where the Kiln process can run `kubectl apply -f -` against the target cluster. Configure `[execution].callback_url` and `KILN_AGENT_CALLBACK_SECRET`; kubectl mode fails startup without them so launched Jobs can complete through authenticated callbacks. Unknown execution modes fail startup. Local commands and generated Kubernetes Jobs receive run metadata, repository/PR metadata, command text, opaque agent/model values, callback metadata, and the configured fallback runtime image as environment variables. `launch_timeout_seconds` bounds local process and kubectl launch calls; `stale_run_seconds` lets Kiln fail stale running jobs and advance the per-PR queue.

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

Use `"status": "failed"` to mark a run failed. `KILN_CALLBACK_TOKEN` is unique to the run and derived from Kiln's private callback key; the shared callback key is never passed to agents. A successful callback updates GitHub-backed run state and advances the next queued run for the PR.

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

## Deployment

For real GitHub delivery, expose the service at a stable HTTPS URL and configure the GitHub App webhook URL to:

```text
https://<your-host>/webhooks/github
```

The service expects GitHub `issue_comment` events. Non-actionable events return `200` with an ignored reason so GitHub does not retry them.

Minimum tested GitHub App repository permissions:

- Checks: read and write.
- Issues: read and write.
- Pull requests: read and write.
- Metadata: read-only.

Subscribe the app to the `Issue comment` event. GitHub emits main pull request conversation comments as `issue_comment` events because pull requests are issues in GitHub's data model.

## Local GitHub Testing

A tunnel is only needed if you want GitHub.com to send webhooks to a process running on your laptop. The default workflow avoids that by using simulated HTTP tests.

See `docs/integration-testing.md` for the reusable manual GitHub App test setup.

## OpenCode Agent Example

Kiln includes runnable examples for using OpenCode as a real agent in both local and Kubernetes execution modes. The examples run `opencode run`, post the output back to the PR, and let Kiln manage queue state and Check Runs.

Start here:

```text
docs/opencode-agent.md
```

## Current Boundaries

- The default execution mode is disabled; local and Kubernetes execution require explicit config.
- Local execution has no sandbox and runs with the Kiln process user's host permissions.
- Kiln generates runtime job metadata, but the agent harness container is not implemented in this repository yet.
- Kiln detects devcontainer intent in helper code, but does not run Devcontainer CLI itself.
- Model routing and agent/model validation belong to the agent harness.
- Database-backed state is intentionally out of scope; GitHub remains the source of truth.
