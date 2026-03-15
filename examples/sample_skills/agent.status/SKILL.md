---
name: agent.status
version: 1.0.0
description: Return minimal runtime status
kind: builtin
entrypoint: agent.status
input_schema:
  type: object
output_schema:
  type: object
permissions: []
timeout_ms: 400
max_retries: 0
cache_ttl_secs: 2
idempotent: true
side_effects: none
tags: [agent, status]
triggers: [status, health, uptime]
---
## What it does
Returns a compact status payload including current UTC time.

## When to use
Use when user asks if the agent is alive or operational.

## When NOT to use
Do not use for detailed diagnostics.

## Input notes
No required fields.

## Output notes
Returns a status object.

## Failure handling
Should only fail if runtime state is invalid.

## Examples
{}
