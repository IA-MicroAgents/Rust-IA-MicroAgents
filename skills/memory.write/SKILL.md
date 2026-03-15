---
name: memory.write
version: 1.0.0
description: Persist a stable fact into long-term memory
kind: builtin
entrypoint: memory.write
input_schema:
  type: object
  properties:
    key: { type: string }
    value: { type: string }
    confidence: { type: number }
  required: [key, value]
output_schema:
  type: object
permissions: []
timeout_ms: 500
max_retries: 0
cache_ttl_secs: 0
idempotent: false
side_effects: writes to facts table
tags: [memory, facts]
triggers: [remember, save, preference, profile]
---
## What it does
Writes a structured fact into durable memory.

## When to use
Use when the user states a stable preference or durable profile fact.

## When NOT to use
Do not use for transient context or high-risk sensitive data.

## Input notes
`key` and `value` are required strings. `confidence` is optional.

## Output notes
Returns `{ ok, key, value, confidence }`.

## Failure handling
Fails on schema validation or storage errors.

## Examples
{"key":"preferred_language","value":"Spanish","confidence":0.9}
