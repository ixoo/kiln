# Integration Testing

Kiln's normal validation path is local simulated HTTP tests with a mocked `GitHubClient`. Real GitHub App integration testing is manual and should use a private throwaway repository plus credentials stored outside this repo.

Do not store GitHub App secrets, webhook secrets, private keys, or tunnel URLs in git.

## Existing Ixoo Setup

The current reusable test setup is:

- GitHub test repository: `ixoo/kiln-test`
- Test pull request: `https://github.com/ixoo/kiln-test/pull/1`
- GitHub App name: `Kiln Test`
- GitHub App ID: `3677034`
- GitHub App slug: `kiln-test`
- Installed repositories: `ixoo/kiln-test` only

Use this setup only if you have access to the app private key and webhook secret. Otherwise create your own setup with the steps below.

## Create Your Own Setup

Use placeholders consistently:

- `<owner>`: your GitHub user or organization.
- `<repo>`: a private test repo, for example `kiln-test`.
- `<app-name>`: a unique GitHub App name, for example `kiln-test-<username>`.
- `<config-dir>`: your local integration config directory, for example `~/.config/kiln-test`.

1. Create a private test repository:

```sh
gh repo create <owner>/<repo> --private --description "End-to-end test repository for Kiln GitHub App webhooks" --add-readme
```

2. Create a pull request to receive `/agent` comments:

```sh
gh repo clone <owner>/<repo> /tmp/<repo>
git -C /tmp/<repo> switch -c kiln-e2e-smoke
printf '# Kiln Smoke Test\n\nThis file exists to open a test PR.\n' > /tmp/<repo>/smoke-test.md
git -C /tmp/<repo> add smoke-test.md
git -C /tmp/<repo> commit -m "Add smoke test file"
git -C /tmp/<repo> push -u origin kiln-e2e-smoke
gh pr create --repo <owner>/<repo> --base main --head kiln-e2e-smoke --title "Kiln webhook smoke test" --body "Test PR for Kiln GitHub App webhook handling."
```

3. Generate a webhook secret:

```sh
openssl rand -hex 32
```

Use the same command to generate `KILN_STATE_SECRET` and `KILN_AGENT_CALLBACK_SECRET` values for the local env file later.

4. Start a Cloudflare Quick Tunnel:

```sh
cloudflared tunnel --url http://127.0.0.1:3000
```

5. Copy the generated `https://*.trycloudflare.com` URL.

6. Create a GitHub App at `https://github.com/settings/apps/new`.

7. Configure the app:

```text
GitHub App name: <app-name>
Homepage URL: https://github.com/<owner>/kiln
Webhook URL: https://<generated-host>/webhooks/github
Webhook secret: <webhook-secret-from-step-3>
```

8. Configure repository permissions:

- Checks: read and write
- Issues: read and write
- Pull requests: read and write
- Metadata: read-only

9. Subscribe to events:

- Issue comment

GitHub emits main pull request conversation comments as `issue_comment` events because pull requests are issues in GitHub's data model. Kiln filters these events and only acts when `issue.pull_request` is present.

10. Create the app and record the App ID.

11. Generate a private key from the app settings page.

12. Install the app only on `<owner>/<repo>`.

The GitHub App Client ID and Client secret are not used by Kiln.

## Local Config Directory

Keep each integration-test setup in one user config directory outside this repository:

```text
~/.config/kiln-test/
|-- .env
|-- kiln.toml
`-- keys/
    `-- kiln-test.private-key.pem
```

Do not move private keys into the project tree.

Create the directory:

```sh
mkdir -p ~/.config/kiln-test/keys
chmod 700 ~/.config/kiln-test ~/.config/kiln-test/keys
```

Place the GitHub App private key in the config directory:

```sh
mv <private-key-source>.pem ~/.config/kiln-test/keys/kiln-test.private-key.pem
```

Create `~/.config/kiln-test/kiln.toml`:

```toml
[server]
bind_address = "127.0.0.1:3000"

[execution]
mode = "disabled"
namespace = "default"
job_image = "ghcr.io/ixoo/kiln-agent:latest"
default_runtime_image = "ghcr.io/devcontainers/base:ubuntu"
# local_command = ["kiln-agent"]
# callback_url = "https://<generated-host>/callbacks/agent"
# job_env_from_secret = "kiln-opencode-agent"
```

Create `~/.config/kiln-test/.env` with absolute paths:

```sh
KILN_CONFIG=/Users/<user>/.config/kiln-test/kiln.toml
KILN_GITHUB_APP_ID=<app-id>
KILN_GITHUB_WEBHOOK_SECRET=<app-webhook-secret>
KILN_STATE_SECRET=<state-marker-secret>
KILN_PREVIOUS_STATE_SECRETS=
KILN_AGENT_CALLBACK_SECRET=<agent-callback-secret>
KILN_GITHUB_PRIVATE_KEY_PATH=/Users/<user>/.config/kiln-test/keys/kiln-test.private-key.pem
RUST_LOG=kiln=info
```

Lock down local files:

```sh
chmod 600 ~/.config/kiln-test/.env ~/.config/kiln-test/kiln.toml ~/.config/kiln-test/keys/*.pem
```

## Run The Test

1. Start Kiln locally:

```sh
KILN_ENV_FILE=$HOME/.config/kiln-test/.env cargo run
```

2. Start or keep running a Cloudflare Quick Tunnel in another terminal:

```sh
cloudflared tunnel --url http://127.0.0.1:3000
```

3. If the tunnel URL changed, update the GitHub App webhook URL:

```text
https://<generated-host>/webhooks/github
```

4. Verify the tunnel reaches Kiln:

```sh
curl https://<generated-host>/healthz
```

5. Post a command on the test PR:

```sh
gh pr comment <pr-number> --repo <owner>/<repo> --body "/agent ping"
```

The GitHub user posting the command must have `write`, `maintain`, or `admin` permission on the test repository. Lower permissions produce a rejection comment instead of a Check Run.

6. Confirm the expected result:

- The GitHub App bot posts an acknowledgement comment.
- A Check Run named `kiln/harness default (<run-id>)` appears on the PR.
- The run ID starts with `kiln_`.
- The GitHub App webhook delivery for `issue_comment.created` returns `200 OK`.

## Reuse Later

For later manual runs with the same app and repo:

1. Start Kiln with `KILN_ENV_FILE=$HOME/.config/kiln-test/.env cargo run`.
2. Start a Cloudflare Quick Tunnel.
3. Update the GitHub App webhook URL if the tunnel URL changed.
4. Comment `/agent ping` on the existing test PR or open a new PR.

Cloudflare Quick Tunnel URLs are temporary. For automated recurring integration tests, use a stable HTTPS deployment or a named Cloudflare tunnel, then store the GitHub App credentials in a secret manager or CI secrets. Do not add those credentials to this repository.

For a real OpenCode agent smoke test, use `docs/opencode-agent.md` after this GitHub App setup works in `disabled` mode.
