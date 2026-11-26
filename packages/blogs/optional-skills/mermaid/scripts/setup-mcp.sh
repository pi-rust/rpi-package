#!/usr/bin/env bash
# Ensure the official Mermaid MCP server is configured for Claude Code.
# Idempotent: does nothing if already present; adds it (user scope) if missing.
set -euo pipefail

MCP_NAME="mermaid"
MCP_URL="https://mcp.mermaid.ai/mcp"

if ! command -v claude >/dev/null 2>&1; then
  echo "ERROR: 'claude' CLI not found on PATH. Configure the Mermaid MCP manually:"
  echo "  claude mcp add --transport http --scope user ${MCP_NAME} ${MCP_URL}"
  exit 1
fi

# 'claude mcp get <name>' exits non-zero if the server isn't configured.
if claude mcp get "${MCP_NAME}" >/dev/null 2>&1; then
  echo "OK: Mermaid MCP server '${MCP_NAME}' is already configured."
  echo "Verify it is connected with: claude mcp list"
  exit 0
fi

echo "Mermaid MCP server not found. Adding '${MCP_NAME}' (${MCP_URL}) at user scope..."
claude mcp add --transport http --scope user "${MCP_NAME}" "${MCP_URL}"

echo
echo "ADDED. IMPORTANT: restart Claude Code (or run /mcp) to activate the new server."
echo "Then confirm with: claude mcp list   (look for '${MCP_NAME} ... Connected')"
