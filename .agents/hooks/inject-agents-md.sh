#!/bin/bash
# .agents/hooks/inject-agents-md.sh

# 1. Find the file
AGENTS_FILE="$(git rev-parse --show-toplevel 2>/dev/null)/AGENTS.md"

# 2. Failsafe: If the file doesn't exist, return empty context
if [ ! -f "$AGENTS_FILE" ]; then
  echo '{"injectSteps": []}'
  exit 0
fi

# 3. Read the contents
content=$(cat "$AGENTS_FILE")

# 4. Use jq to safely escape all quotes, newlines, and special characters
jq -n --arg body "$content" '{
  "injectSteps": [
    {
      "ephemeralMessage": "## AGENTS.md — mandatory context, follow every rule below\n\n\($body)"
    }
  ]
}'

exit 0