# Mapa de Migración `in / usecase / out`

## Objetivo
Dejar Ferrum organizado en tres capas:

- `src/in`
- `src/usecase`
- `src/out`

## Estado actual

### Ya migrado
- Caso de uso principal:
  - [src/usecase/process_inbound_event.rs](/Users/yasnielfajardo/Documents/PROJECTS/open-agent-team/src/usecase/process_inbound_event.rs)
- Caso de uso de reminders:
  - [src/usecase/send_reminder.rs](/Users/yasnielfajardo/Documents/PROJECTS/open-agent-team/src/usecase/send_reminder.rs)
- Compatibilidad legacy del orquestador:
  - [src/orchestrator/loop.rs](/Users/yasnielfajardo/Documents/PROJECTS/open-agent-team/src/orchestrator/loop.rs)
- Facades de entrada:
  - [src/in/cli.rs](/Users/yasnielfajardo/Documents/PROJECTS/open-agent-team/src/in/cli.rs)
  - [src/in/http.rs](/Users/yasnielfajardo/Documents/PROJECTS/open-agent-team/src/in/http.rs)
  - [src/in/telegram.rs](/Users/yasnielfajardo/Documents/PROJECTS/open-agent-team/src/in/telegram.rs)
- Facades de salida:
  - [src/out/llm.rs](/Users/yasnielfajardo/Documents/PROJECTS/open-agent-team/src/out/llm.rs)
  - [src/out/persistence.rs](/Users/yasnielfajardo/Documents/PROJECTS/open-agent-team/src/out/persistence.rs)
  - [src/out/events.rs](/Users/yasnielfajardo/Documents/PROJECTS/open-agent-team/src/out/events.rs)
  - [src/out/telegram.rs](/Users/yasnielfajardo/Documents/PROJECTS/open-agent-team/src/out/telegram.rs)

## Pendiente por migrar

### `in`
- adaptar [src/app.rs](/Users/yasnielfajardo/Documents/PROJECTS/open-agent-team/src/app.rs) para depender más explícitamente de `src/in`
- mover handlers HTTP finos fuera de módulos legacy

### `usecase`
- extraer casos de uso de:
  - planificación
  - integración final
  - export de trazas
  - replay
  - chat local
  - configuración de team/dashboard

### `out`
- aislar más claramente:
  - Postgres
  - Redis cache
  - Redis Streams
  - OpenRouter
  - Telegram outbound

## Regla operativa
Todo código nuevo debe entrar ya en esta forma.  
El código viejo solo se toca para delegar hacia la capa nueva.
