---
name: memory.search
version: 1.0.0
description: Search indexed memory documents and turns
kind: builtin
entrypoint: memory.search
input_schema:
  type: object
  properties:
    query: { type: string }
    limit: { type: integer, minimum: 1, maximum: 20 }
  required: [query]
output_schema:
  type: object
permissions: []
timeout_ms: 600
max_retries: 0
cache_ttl_secs: 10
idempotent: true
side_effects: none
tags: [memory, search, retrieval]
triggers: [recall, remember, search, find]
---
## What it does
Runs SQLite FTS retrieval over conversation memory.

## When to use
Use for recall questions or context lookup before answering.

## When NOT to use
Do not use when exact external real-time data is required.

## Input notes
`query` required, `limit` optional.

## Output notes
Returns `{ results: [...] }` ordered by relevance.

## Failure handling
Fails on validation or FTS query issues.

## Examples
{"query":"project deadline","limit":5}
