# AI MicroAgents Next-Level Master Plan

## Objetivo
Llevar `AI MicroAgents` de un runtime sólido y operable a una plataforma de orquestación visual, multimodal y claramente superior en Telegram, sin perder sus principios de determinismo, control de costos y simplicidad operativa.

## Qué copiaría de OpenClaw

### 1. Canal Telegram más profundo
- Soporte completo para mensajes privados, grupos y topics.
- Modos de entrada por política: `pairing`, `allowlist`, `open`, `disabled`.
- Routing determinista por chat, grupo y topic.
- Registro automático de comandos nativos de Telegram.
- `reply threading` explícito para responder al mensaje correcto.
- Streaming de respuesta por edición del mismo mensaje o borrador nativo cuando aplique.

### 2. Pipeline de media real
- Inbound media normalizado como adjuntos, no solo texto.
- Descarga controlada de archivos con tamaño máximo, MIME validado y TTL.
- Inserción de bloques estructurados de contexto para el agente:
  - `[Image]`
  - `[Audio]`
  - `[Video]`
- Transcripción y descripción antes de la decisión de routing.
- Preservar caption original y mezclarlo con el análisis del medio.

### 3. Dashboard como control plane, no solo monitor
- Vista de inbox.
- Vista de sesión/conversación.
- Vista de flujo de ejecución.
- Vista de plan y DAG.
- Vista de costos, tokens y latencias.
- Vista de subagentes persistentes y efímeros.
- Operaciones de runtime: pause, replay, reload, retry, cancel.

### 4. Experiencia de tiempo real mejor lograda
- Eventos con semántica clara, no solo log crudo.
- Streaming visual del trabajo paralelo.
- Estado de draft / progreso / review / integración / entrega.
- Tiping/progress indicators en Telegram para tareas largas.

## Qué NO copiaría de OpenClaw
- Multiplicidad masiva de canales al mismo tiempo.
- Dependencia fuerte en UI compleja tipo app-suite.
- Mezcla excesiva de nodos, móviles y superficies externas en v1.
- Arquitectura demasiado amplia para una sola persona.

## Dirección del producto

### A. Telegram-first multimodal
`AI MicroAgents` debe entender y responder a:
- texto
- imagen
- audio / voice note
- documento liviano con OCR opcional

`AI MicroAgents` debe generar:
- texto
- imagen
- opcionalmente audio de salida en una fase posterior

### B. Dashboard tipo mission-control
El dashboard debe sentirse como una sala de control:
- `Home`: salud, actividad, costos, alertas, canal, colas.
- `Inbox`: mensajes entrantes y estado de cada conversación.
- `Flow`: canvas full-screen con supervisor, workers, tareas, revisiones y entrega.
- `Trace`: timeline detallado por conversación.
- `Team`: subagentes persistentes y efímeros.
- `Media`: adjuntos recibidos, análisis y artefactos generados.
- `Config`: identidad, skills, modelos, budgets, policies.

## Plan visual del dashboard

### Fase 1. Base UX
- Mobile first real.
- Bottom nav en móvil, sidebar en desktop.
- Jerarquía visual fuerte: hero + métricas + alertas + actividad.
- Mejor contraste y separación entre overview, flow y events.
- Scroll horizontal solo dentro del canvas, nunca global.

### Fase 2. Canvas serio
- Canvas SVG con capas:
  - capa de grid
  - capa de edges
  - capa de packets animados
  - capa de nodos
  - capa de overlays
- Nodos canónicos:
  - Telegram mobile/chat
  - Supervisor
  - Worker pool persistente
  - Worker pool efímero
  - Plan tasks
  - Review lane
  - Integration node
  - Final delivery
- Edge types:
  - control
  - assignment
  - dependency
  - artifact
  - review
  - delivery

### Fase 3. Animaciones con semántica
- Inbound Telegram: burbuja móvil -> supervisor.
- Delegación: supervisor -> subagente.
- Burst worker: aparición animada desde pool efímero.
- Artifact: subagente -> review lane.
- Retry/reject: retorno rojo al nodo tarea.
- Accept: transición verde hacia integración.
- Final send: supervisor -> Telegram.

### Fase 4. Interacción
- Pan/zoom/minimap.
- Click en nodo para abrir panel lateral.
- Click en edge para ver evento relacionado.
- Filtros por conversación, plan, task state, role, canal.
- Export de trace bundle desde panel lateral.

## Telegram multimodal: diseño objetivo

### Inbound
Normalizar estos tipos:
- `text`
- `photo`
- `audio`
- `voice`
- `document`
- `video` después

Estructura sugerida:
- `attachments[]`
  - `id`
  - `kind`
  - `mime`
  - `size_bytes`
  - `telegram_file_id`
  - `telegram_file_unique_id`
  - `caption`
  - `storage_url | local_path`
  - `sha256`

### Pipeline sugerido
1. recibir update
2. persistir raw update
3. normalizar metadata
4. descargar archivo con `getFile`
5. validar tamaño/MIME
6. guardar en object store local o S3-compatible
7. ejecutar análisis multimodal
8. enriquecer contexto del turno
9. pasar al router/orchestrator
10. responder por Telegram según tipo de salida

## Herramientas recomendadas

### Telegram en Rust
Recomendación principal:
- `teloxide`

Motivo:
- es el framework Rust más maduro y común para bots de Telegram
- soporta long polling y webhooks
- permite configurar cliente HTTP y shutdown limpio
- encaja mejor que mantener cliente Bot API hecho a mano cuando empieces con media, comandos, callbacks y grupos

### Imagen: comprensión
Recomendación principal:
- OpenRouter multimodal para vision

Uso:
- enviar imagen como `image_url` o base64
- usar ruta/modelo `vision`
- aprovechar el mismo provider layer actual

### Audio: comprensión / transcripción
Recomendación base para mantener simplicidad del stack:
- OpenRouter `input_audio` para audio reasoning/transcripción simple

Recomendación premium para transcripts de alta calidad y diarización:
- OpenAI `gpt-4o-transcribe`
- OpenAI `gpt-4o-transcribe-diarize`

Alternativa muy fuerte si priorizas streaming STT de baja latencia:
- Deepgram Nova-3

### Generación de imágenes
Opción preferida dentro del stack actual:
- OpenRouter image generation con modelos que soporten `output_modalities: ["image"]`

Opciones fuertes:
- `google/gemini-3.1-flash-image-preview`
- `black-forest-labs/flux.2-pro`
- `black-forest-labs/flux.2-flex`

Opción premium fuera de OpenRouter si quieres máxima adherencia y editing avanzado:
- OpenAI `gpt-image-1.5`

## Decisión recomendada

### Para la siguiente iteración
Implementar esto, en este orden:
1. Migrar cliente Telegram a `teloxide`.
2. Añadir `attachments[]` al envelope normalizado.
3. Implementar descarga `getFile` + storage local/S3-compatible.
4. Soporte inbound `photo`, `audio`, `voice`, `document`.
5. Añadir pipeline de análisis multimodal:
   - imagen -> OpenRouter vision
   - audio corto -> OpenRouter audio
   - audio importante/largo -> OpenAI transcription backend opcional
6. Añadir skill de generación de imágenes.
7. Añadir envío outbound `sendPhoto`.
8. Rehacer dashboard como control-plane visual con canvas SVG real.

## Roadmap por fases

### Fase 1. Telegram media foundation
- `photo`, `voice`, `audio`, `document`
- `getFile`
- límites de tamaño
- persistencia de adjuntos
- análisis de imagen
- transcripción de audio

### Fase 2. Output multimodal
- `sendPhoto`
- skill `image.generate`
- respuesta mixta `text + image`
- moderación de imagen antes de enviar

### Fase 3. Dashboard v2
- layout app-shell
- flow canvas full-screen
- timeline y trace panel
- subagentes efímeros visibles
- costos y latencias en tiempo real

### Fase 4. Telegram advanced UX
- comandos nativos
- callbacks/inline buttons
- grupos/topics
- reply threading
- streaming preview/progress

## Riesgos y criterios

### Riesgos
- Bot API media + archivos puede aumentar costo de almacenamiento rápido.
- Audio largo requiere chunking o backend especializado.
- No conviene meter video en la misma fase que audio/imágenes.
- El dashboard puede volverse pesado si no se separan bien vistas de overview y trace.

### Criterios de aceptación
- una foto enviada al bot produce descripción útil y respuesta contextual
- un voice note produce transcript y respuesta contextual
- una imagen generada puede ser enviada de vuelta por Telegram
- el canvas muestra claramente inbound -> routing -> parallel tasks -> review -> final delivery
- no hay scroll global roto ni pérdida de rendimiento en móvil
