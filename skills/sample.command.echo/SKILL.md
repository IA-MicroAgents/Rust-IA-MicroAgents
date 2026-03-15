---
name: sample.command.echo
version: 1.0.0
description: Example JSON stdin/stdout command skill
kind: command
entrypoint: echo_skill.sh
input_schema:
  type: object
output_schema:
  type: object
permissions: []
timeout_ms: 1200
max_retries: 0
cache_ttl_secs: 0
idempotent: true
side_effects: none
tags: [sample, command]
triggers: [echo sample]
---
## What it does
Demonstrates the external command protocol used by ferrum.

## When to use
Use for validating command-skill wiring in development.

## When NOT to use
Do not use in production-facing workflows.

## Input notes
Any JSON object is accepted.

## Output notes
Returns `{ ok: true, echoed: <input> }`.

## Failure handling
Fails when command exits non-zero or emits invalid JSON.

## Examples
{"hello":"world"}
