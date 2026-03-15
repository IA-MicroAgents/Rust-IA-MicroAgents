---
name: agent.help
version: 1.0.0
description: Return help hints about AI MicroAgents capabilities
kind: builtin
entrypoint: agent.help
input_schema:
  type: object
output_schema:
  type: object
permissions: []
timeout_ms: 400
max_retries: 0
cache_ttl_secs: 5
idempotent: true
side_effects: none
tags: [agent, help]
triggers: [help, what can you do, capabilities]
---
## What it does
Returns guidance for supported tools and safe operating patterns.

## When to use
Use when user asks about capabilities or usage.

## When NOT to use
Do not use as a substitute for executing a specific task.

## Input notes
No required fields.

## Output notes
Returns structured help hints.

## Failure handling
Should not fail in normal operation.

## Examples
{}
