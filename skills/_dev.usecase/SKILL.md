---
name: dev-usecase-layer
description: Usar cuando haya que crear o refactorizar casos de uso del sistema siguiendo la capa usecase de AI MicroAgents.
---

# Capa `usecase`

## Objetivo
La capa `usecase` expresa el negocio como pasos claros y aislados.

## Qué entra aquí
- procesamiento inbound
- planificación
- ejecución coordinada
- integración
- casos de uso de reminders
- flujos de memoria

## Reglas obligatorias
1. el caso de uso debe tener una entrada principal tipo `execute`
2. cada bloque importante debe tener comentario en español
3. la lógica no debe depender de detalles HTTP/Telegram/Postgres/Redis/OpenRouter
4. el caso de uso coordina, no implementa infraestructura

## Plantilla mental
1. validar
2. cargar contexto
3. decidir
4. ejecutar
5. persistir
6. entregar
