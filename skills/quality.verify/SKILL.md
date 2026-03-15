---
name: quality.verify
version: 1.0.0
description: Deterministic quality check against acceptance criteria
kind: builtin
entrypoint: quality.verify
input_schema:
  type: object
  properties:
    content: { type: string }
    criteria:
      type: array
      items: { type: string }
  required: [content]
output_schema:
  type: object
permissions: []
timeout_ms: 600
max_retries: 0
cache_ttl_secs: 0
idempotent: true
side_effects: none
tags: [quality, verify, review]
triggers: [verify, quality check, acceptance criteria]
---
## What it does
Scores generated content against acceptance criteria using deterministic lexical checks.

## When to use
Use before accepting task artifacts or final responses.

## When NOT to use
Do not use as a factuality oracle for external truth.

## Input notes
`content` is required. `criteria` is optional and defaults to empty.

## Output notes
Returns `{ ok, score, results[] }`.

## Failure handling
Fails on invalid schema input.

## Examples
{"content":"Answer includes test evidence","criteria":["evidence","answer"]}
