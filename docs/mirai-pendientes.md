# Mirai Assistant — pendientes para retomar

**Fecha:** 2026-10-24. **Estado:** primera adaptación publicada; todavía no hay panel dentro de GridCraft ni DeckCraft.

## Código publicado

Rama de trabajo en ambos repositorios: `feature/claude-assistant`. No se modificó `main`.

| Aplicación | Repositorio | Commit de la adaptación |
|---|---|---|
| GridCraft | https://github.com/mirasins/gridcraft-Mirai-Fork/tree/feature/claude-assistant | `0c46cfdf` |
| Presentations / DeckCraft | https://github.com/mirasins/deckcraft-Mirai-Fork/tree/feature/claude-assistant | `6ebeb09` |

Ya implementado en ambos:

- Cliente de Claude Code oficial en `crates/assistant`, con límites, cancelación y respuestas estructuradas.
- Comandos del motor `assistant.context` y `assistant.apply`: snapshot acotado, validación de documento/revisión/originales y cambios deshacibles.
- Comando CLI `mirai`: genera una **vista previa JSON**, sin aplicar cambios ni guardar archivos.
- Documentación técnica y limitaciones en [mirai-assistant.md](mirai-assistant.md).

**Importante:** el snapshot pertenece a la sesión que lo creó. La salida de la CLI no es un parche que pueda aplicarse después de cerrar y reabrir el archivo.

## Prioridad 1 — asistente usable dentro de las aplicaciones

- [ ] Incorporar el panel de Mirai a GridCraft y DeckCraft, con una entrada visible para abrirlo/cerrarlo.
- [ ] Conectar selección → contexto → solicitud a Claude → propuesta → revisión → aplicación, manteniendo viva la misma sesión.
- [ ] Mostrar originales y propuestas; permitir aceptar/rechazar explícitamente antes de aplicar.
- [ ] Ejecutar generación fuera del hilo de interfaz; mostrar progreso, cancelación y errores comprensibles.
- [ ] Invalidar propuestas al cambiar de documento o revisión; no aplicar silenciosamente a otra selección.
- [ ] Comprobar deshacer/rehacer, protección de celdas y bloqueo de formas desde el panel real.

## Prioridad 2 — look común morado/lavanda

Petición del usuario: **Mirai Assistant debe compartir el mismo aspecto en WordCraft, GridCraft y Presentations**.

- [ ] Usar como referencia la versión actual del asistente de WordCraft: superficies lavanda, acciones moradas y variantes clara/oscura.
- [ ] Mantener distribución conversacional, cuadro de entrada amplio/multilínea y selector de modelos desplegable.
- [ ] Unificar nombre visible: **Mirai Assistant**. No renombrar las aplicaciones anfitrionas.
- [ ] Coordinar los cambios con el otro asistente que está trabajando en la estética de WordCraft; no sobrescribir su trabajo.
- [ ] Revisar capturas headless de los paneles, tamaños estrechos, escalado, foco de teclado y contraste.
- [ ] Si se reutiliza el pájaro original autorizado por el usuario, registrar sus archivos y licencia en `ATTRIBUTION.md` de cada fork.

## Prioridad 3 — validación e integración

- [ ] Probar ambos asistentes con una cuenta real iniciada en Claude Code. No se hizo ninguna solicitud real en esta adaptación.
- [ ] Verificar conexión, generación, revisión, aplicación, undo/redo, cancelación, cuenta no válida y propuestas obsoletas.
- [ ] Añadir pruebas de integración del panel y repetir `cargo xtask ci` tras cada arco.
- [ ] Probar accesibilidad y funcionamiento en macOS/Linux; la validación realizada aquí fue en Windows.
- [ ] Preparar ejecutables/paquetes de evaluación solo cuando exista la integración visual. No se generaron paquetes nuevos de estos forks.

## Bloqueos y resultados conocidos

### GridCraft: experimental, CI completo pendiente

- Pasan 31 pruebas del cliente, 15 del motor de Mirai y 8 de su CLI.
- Pasan formato, Clippy del workspace, WASM, doctests, capas, atribución y comprobaciones generadas de parity/keytips.
- **Fallo de cálculo preexistente confirmado en baseline `3099df93`:** el fuzz encuentra diferencias entre recálculo incremental y completo al colisionar derrames de fórmulas.
- Reproducción mínima sobre el sample `sales`:

```text
cell.set {"cell":"C14","input":"=TRANSPOSE(B18:B24)","sheet":"Sales"}
range.fill {"input":"=D14:E18","range":"E13:F13"}
```

- `F13` queda con `#SPILL!` incrementalmente, pero cambia tras un recálculo completo. **No se corrigió ni se alteró la semántica del cálculo.** Resolver la precedencia entre derrames requiere una decisión de diseño; no cambiarla solo para conseguir CI verde.
- **Otro fallo pendiente:** `header_autofit::table_filter_autofit_works_through_command_and_header_double_click`, solape de cabecera/filtro al 50 % de zoom. También falla aisladamente; su atribución al baseline quedó pendiente. En la ejecución conjunta pasaron 768 de 769 pruebas de integración UI.
- No ignorar estas pruebas ni presentar Grid como totalmente validado. Se publicó experimental por petición expresa del usuario.

### DeckCraft: CI pasa con OpenGL para las pruebas

- Pasan 31 pruebas del cliente, 8 del motor de Mirai y 6 de su CLI.
- **Las ocho etapas de `cargo xtask ci` pasan con `WGPU_BACKEND=gl`.** WASM comprueba 24 crates.
- El backend GPU predeterminado de Windows produjo `0xc0000005` en pruebas de UI paralelas. El mismo binario pasa en serie (277 pruebas activas) y en paralelo con OpenGL. El diagnóstico apunta a renderizados wgpu nativos concurrentes; no se modificaron el código UI ni sus dependencias GPU para Mirai.
- Esta limitación **no se corrigió** ni se cambió la configuración gráfica de producción. Documentar siempre el backend usado para validar.
- Falta el checkout opcional `craft-fonts`; sus advertencias siguen presentes. No se evaluó aquí el render con ese paquete de fuentes.

## Restricciones que deben conservarse

- Solo Claude Code oficial y autenticación de cuenta oficialmente soportada; sin API keys, proveedores alternativos, servidores propios ni extracción/copia de credenciales.
- El contenido del documento es dato no confiable. Herramientas de Claude deshabilitadas; no ejecutar órdenes derivadas de ese texto.
- Límites actuales: 256 objetivos y 64 KiB de contexto/datos acotados por el motor; respetar además los límites del cliente.
- Grid: las sugerencias de texto se guardan como texto literal, incluso si empiezan por `=`. Las fórmulas son una operación separada con permiso explícito; el cliente actual no las genera.
- Deck: alcance inicial limitado al texto completo de formas simples de la diapositiva actual. Sin notas, tablas, imágenes, grupos recursivos ni generación de diseños.
- No prometer conversación multivuelta: no se implementó el envío de historial como contexto.

## Orden recomendado al retomar

1. Leer `AGENTS.md`, este archivo y `docs/mirai-assistant.md` en cada fork.
2. Separar los fallos heredados de Grid del trabajo del asistente; acordar el tratamiento de la colisión de derrames antes de modificar el cálculo.
3. Implementar un arco pequeño del panel lavanda y el flujo revisión/aplicación en una aplicación; comprobarlo antes de trasladar el patrón a la otra.
4. Hacer las pruebas reales de cuenta y documentar exactamente qué está implementado, validado y pendiente.
