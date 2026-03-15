---
name: dev-out-layer
description: Usar cuando haya que crear o refactorizar adapters de salida e infraestructura siguiendo la capa out de AI MicroAgents.
---

# Capa `out`

## Objetivo
La capa `out` encapsula infraestructura y sistemas externos.

## Qué entra aquí
- Postgres
- Redis
- OpenRouter
- Telegram outbound
- event bus
- streams y colas secundarias

## Qué debe hacer
1. encapsular detalles técnicos
2. exponer contratos claros hacia usecase
3. mapear errores técnicos
4. aislar proveedores

## Qué no debe hacer
- reglas de negocio
- decisiones del supervisor
- mezclar intención funcional con detalles técnicos
