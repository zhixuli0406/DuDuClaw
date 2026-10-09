FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive \
    GLAMA_VERSION="1.0.0" \
    PYTHONUNBUFFERED=1

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
    && curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && npm install -g mcp-proxy@6.7.16 pnpm@10.14.0 \
    && node --version \
    && curl -LsSf https://astral.sh/uv/install.sh | UV_INSTALL_DIR="/usr/local/bin" sh \
    && uv python install 3.14 --default --preview \
    && ln -sf "$(uv python find)" /usr/local/bin/python \
    && ln -sf "$(uv python find)" /usr/local/bin/python3 \
    && python --version \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/* /tmp/* /var/tmp/*

WORKDIR /app

COPY . /app

ENV PATH="/app/node_modules/.bin:$PATH"

RUN npm install \
    && npm install -g . \
    && duduclaw --version

# mcp-proxy 6.7.16 listens on IPv6 "::" port 8080 and does not read PORT.
# A gateway that dials the container's IPv4 address, or probes $PORT, gets
# an immediate 502. Exec form cannot expand these variables.
# MCP_PROXY_HOST is a backstop for a regenerated CMD that drops the flags.
ENV MCP_PROXY_HOST=0.0.0.0
CMD ["sh", "-c", "exec mcp-proxy --host \"${MCP_PROXY_HOST:-0.0.0.0}\" --port \"${MCP_PROXY_PORT:-${PORT:-8080}}\" -- /app/distribution/glama/entrypoint.sh"]
