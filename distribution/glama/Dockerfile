# Container for MCP directories (Glama) that build and run a server to list
# its tools. It runs the standalone MCP server from the npm package; the
# gateway is not included. Build from the repository root:
#
#   docker build -f distribution/glama/Dockerfile -t duduclaw-mcp .
#   docker run -i --rm duduclaw-mcp
#
# Without DUDUCLAW_MCP_API_KEY the entrypoint issues a standalone key (memory
# and wiki scopes only) inside the container's own data directory.
FROM node:22-slim
RUN npm install -g duduclaw && duduclaw --version
COPY distribution/glama/entrypoint.sh /usr/local/bin/duduclaw-mcp-entrypoint
USER node
ENV HOME=/home/node
ENTRYPOINT ["/usr/local/bin/duduclaw-mcp-entrypoint"]
