# Kiln

Kiln lets maintainers ask an agent to work from a pull request comment.

Comment on a PR:

```text
/agent review this PR for bugs
```

Kiln verifies the requester, acknowledges the request, creates a GitHub Check Run, and runs the configured agent path when execution is enabled.

## Command Format

Use one of these forms at the start of a comment line:

```text
/agent <task>
/agent:<agent> <task>
/agent:<agent>:<model> <task>
```

Examples:

```text
/agent fix failing tests
/agent:reviewer review this PR
/agent:build:anthropic/claude-sonnet-4-5 summarize the highest-risk bugs
```

Bare `/agent` leaves agent and model choice to your agent harness. Explicit agent and model values are passed through as metadata.

Commands inside Markdown code fences are ignored, so examples in fenced code blocks are safe to paste.

## What You See

When Kiln accepts a command, GitHub shows:

- An acknowledgement comment from the Kiln GitHub App.
- A Check Run for the agent run.
- Queue ordering for multiple commands on the same PR.
- Running, completed, or failed Check Run status when execution is enabled.

If the requester lacks permission, Kiln posts a rejection comment and does not create a Check Run.

## Who Can Run Agents

The commenter must have one of these repository permissions:

- `write`
- `maintain`
- `admin`

This keeps agent invocation limited to maintainers by default.

## Quick Start

Prerequisites:

- Rust toolchain for running Kiln from source.
- GitHub CLI if you want to post test comments with `gh`.
- A GitHub App installed on a test repository.

1. Create and install a GitHub App for your test repository.

See `docs/integration-testing.md` for the exact GitHub App setup, permissions, webhook secret, and private-key placement.

2. Copy the example config:

```sh
cp config/kiln.example.toml config/kiln.toml
cp .env.example .env
```

3. Edit `.env` with your GitHub App values and generated secrets.

Use a generated secret for each secret value, for example:

```sh
openssl rand -hex 32
```

4. Start Kiln:

```sh
cargo run
```

5. Send GitHub webhooks to Kiln.

For local testing, expose `http://127.0.0.1:3000` with a tunnel and set the GitHub App webhook URL to:

```text
https://<your-host>/webhooks/github
```

6. Comment on a PR:

```sh
gh pr comment <pr-number> --repo <owner>/<repo> --body "/agent ping"
```

The default config uses disabled execution mode, so this smoke test only verifies command parsing, permission checks, acknowledgement comments, and Check Runs.

## Run A Real Agent

Kiln can launch a local process or create a Kubernetes Job with `kubectl`.

The fastest working example is OpenCode:

```text
docs/opencode-agent.md
```

The OpenCode examples run `opencode run`, post the output back to the PR, and let Kiln manage queue state and Check Runs. They do not commit or push code.

## Health Check

```sh
curl http://127.0.0.1:3000/healthz
```

Expected response:

```json
{"status":"ok"}
```

## Documentation

- `docs/integration-testing.md`: create a GitHub App and run an end-to-end smoke test.
- `docs/opencode-agent.md`: run the OpenCode example locally or with Kubernetes.
- `docs/design.md`: architecture, execution modes, callbacks, environment variables, and boundaries.
- `spec.md`: detailed product and implementation specification for maintainers.

## Know The Boundaries

- The default execution mode is disabled.
- Local execution has no sandbox and should only be used on trusted machines and repositories.
- Kubernetes execution requires explicit config and a reachable callback URL.
- Kiln does not provide an agent harness, model router, or commit push-back flow in this repository.
