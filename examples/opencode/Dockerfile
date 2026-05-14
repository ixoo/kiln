FROM node:22-bookworm-slim

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates curl git gh \
  && rm -rf /var/lib/apt/lists/* \
  && npm install -g opencode-ai

COPY agent.sh /usr/local/bin/kiln-opencode-agent
RUN chmod +x /usr/local/bin/kiln-opencode-agent

WORKDIR /work
ENTRYPOINT ["/usr/local/bin/kiln-opencode-agent"]
