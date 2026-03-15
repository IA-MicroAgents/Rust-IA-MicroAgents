---
name: market-data
version: 1.0.0
description: Guidance skill for market data acquisition before any current-market synthesis
kind: command
entrypoint: market_data.sh
input_schema:
  type: object
  properties:
    query: { type: string }
    entities:
      type: array
      items: { type: string }
output_schema:
  type: object
permissions: []
timeout_ms: 500
max_retries: 0
cache_ttl_secs: 0
idempotent: true
side_effects: none
tags: [market, data, evidence]
triggers: [btc usd, market data, precio actual, trading]
---
## What it does
Provides execution guidance for market-data-first flows where live evidence is mandatory before reasoning.

## When to use
Use when the request depends on current market conditions, spot prices, volume, or directional outlook.

## When NOT to use
Do not use for timeless theoretical questions or closed-book comparisons that do not depend on current data.

## Input notes
Accepts a user query and optional asset/entity list.

## Output notes
Returns a small JSON policy describing what evidence to fetch first and what to synthesize later.

## Failure handling
Fails only if the command process cannot be executed.

## Examples
{"query":"BTC/USD al día de hoy","entities":["bitcoin"]}
