---
name: theory-reasoning
version: 1.0.0
description: Guidance skill for deep theoretical, philosophical, and computability reasoning
kind: command
entrypoint: theory_reasoning.sh
input_schema:
  type: object
  properties:
    query: { type: string }
output_schema:
  type: object
permissions: []
timeout_ms: 500
max_retries: 0
cache_ttl_secs: 0
idempotent: true
side_effects: none
tags: [reasoning, theory, proof, philosophy]
triggers: [teorema, computabilidad, moral, halting, proof]
---
## What it does
Provides a decomposition pattern for high-rigor theoretical prompts.

## When to use
Use for CS theory, logic, formal limitations, and philosophical tradeoff questions.

## When NOT to use
Do not use for lightweight factual chat or current-data lookups.

## Input notes
Accepts the raw user query.

## Output notes
Returns JSON with suggested reasoning tracks, validation focus, and desired rigor tier.

## Failure handling
Fails only if the command process cannot be executed.

## Examples
{"query":"Diseña un algoritmo que decida si dos programas arbitrarios son equivalentes"}
