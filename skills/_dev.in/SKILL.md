---
name: dev-in-layer
description: Usar cuando haya que crear o refactorizar adapters de entrada del sistema siguiendo la capa in de AI MicroAgents.
---

# Capa `in`

## Objetivo
La capa `in` adapta entradas externas al lenguaje interno del sistema.

## Qué entra aquí
- handlers HTTP
- webhooks
- polling
- CLI
- adapters de eventos externos

## Flujo esperado
1. recibir input
2. validar forma básica
3. traducir a request/comando del dominio
4. invocar un caso de uso
5. traducir la respuesta al canal

## Qué no debe hacer
- lógica de negocio compleja
- reglas del supervisor
- detalles de persistencia o proveedores si no son estrictamente del adapter
