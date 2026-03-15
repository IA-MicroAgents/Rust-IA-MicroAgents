# Ferrum Architecture Rules

## Objetivo
Todo cambio nuevo en Rust debe seguir una arquitectura de tres capas:

1. `in`
2. `usecase`
3. `out`

La lógica de negocio vive en `usecase`.  
La capa `in` adapta entradas.  
La capa `out` adapta salidas hacia infraestructura y proveedores.

## Reglas de diseño

### Capa `in`
Usar para:
- CLI
- HTTP
- webhooks
- polling
- subscribers de eventos externos

Debe:
- validar forma básica
- traducir input externo a requests del dominio
- invocar un caso de uso

No debe:
- contener reglas de negocio complejas
- decidir estrategia del supervisor
- mezclar persistencia, Redis o OpenRouter con lógica funcional

### Capa `usecase`
Usar para:
- casos de uso
- flujos de negocio
- reglas de decisión
- coordinación entre puertos de salida

Debe:
- leerse como una secuencia de pasos
- aislar la lógica del negocio
- comentar en español cada bloque importante del flujo

No debe:
- parsear detalles de transporte
- conocer detalles técnicos bajos de proveedores

### Capa `out`
Usar para:
- Postgres
- Redis
- OpenRouter
- Telegram outbound
- event bus
- jobs secundarios e integraciones externas

Debe:
- encapsular detalles técnicos
- mapear errores técnicos a errores de la aplicación
- exponer contratos claros para `usecase`

No debe:
- decidir negocio
- inferir intención del usuario

## Regla de migración
Los módulos legacy pueden quedar por compatibilidad, pero:
- deben delegar o reexportar hacia `in`, `usecase` o `out`
- no deben seguir creciendo con lógica nueva

## Comentarios en español en usecases
Cada caso de uso debe comentar los pasos principales con intención de negocio.

Ejemplo correcto:
- `// Paso 3: decidir si la solicitud requiere planificación o se resuelve por fast-path`

Ejemplo incorrecto:
- `// Suma uno al contador`

## Skills locales de arquitectura
Antes de cambios estructurales, leer:
- `skills/_dev.in/SKILL.md`
- `skills/_dev.usecase/SKILL.md`
- `skills/_dev.out/SKILL.md`
