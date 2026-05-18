# TODO: Próximas Funcionalidades para `youtube-to-actions`

Este archivo contiene la hoja de ruta y las ideas de mejora para el proyecto `yt2action`.

---

## 🚀 Funcionalidades Prioritarias

### 1. Configuración de Carpeta Obsidian por Playlist 📂
*   **Descripción:** Permitir especificar una carpeta de destino en Obsidian (`obsidian_folder` o `target_folder`) específica para cada lista de reproducción en el archivo `config.toml`, de modo que los videos de diferentes playlists se clasifiquen en diferentes directorios automáticamente (sin depender únicamente de la decisión del LLM, o bien usándolo como una carpeta base).
*   **Diseño en `config.toml`:**
    ```toml
    [[youtube.playlists]]
    id = "PLAGdrpXgWKzoxMJwIByON_syWny0Ni9tt"
    obsidian_folder = "Proyectos/Rust"
    ```
*   **Cambios requeridos:**
    *   Actualizar `PlaylistPatternConfig` en `src/config.rs` para incluir `pub obsidian_folder: Option<String>`.
    *   Pasar este valor a través de `cmd_run` y `obsidian::create_note`.

---

## 🛠️ Mejoras y Estabilidad

### 2. Soporte para Múltiples Bóvedas de Obsidian (Vaults) 🏛️
*   **Descripción:** Permitir que cada playlist especifique un path de vault distinto si es necesario (`obsidian_vault` a nivel de playlist).

### 3. Fallback inteligente de descargas de Video 🔄
*   **Descripción:** Si `yaydl` falla catastróficamente (exit status 101) al intentar descargar un video y `--download-video` está activo, hacer un fallback automático a `yt-dlp` usando cookies del navegador para evitar bloqueos.

### 4. Modo Concurrente Limitado ⚡
*   **Descripción:** Permitir procesar videos de forma concurrente pero con un limitador estricto (por ejemplo, máx. 2 descargas/análisis a la vez) para no disparar alertas de spam en YouTube o el LLM.

---

### 5. Filtrado por Patrones de Fabric específicos por Playlist 🎨 (Completado)
*   **Descripción:** Integrar el campo `patterns` configurado en cada playlist para que se ejecute ese patrón específico en lugar del predeterminado de la CLI.
*   **Implementación:** Si no se pasa el parámetro `--pattern` a nivel de CLI, el pipeline ejecuta y concatena todos los patrones configurados en la lista de reproducción correspondiente.

### 6. Indicar qué patrón de Fabric se está ejecutando en los logs 🧠 (Completado)
*   **Descripción:** Añadir un mensaje de log explícito que indique qué patrón de Fabric se está ejecutando para analizar el video.
*   **Implementación:** Agregado un log descriptivo antes de ejecutar `process_with_pattern` en `main.rs`.

### 7. Log temporal de salidas de patrones de Fabric 📝 (Para Mañana / Pendiente)
*   **Descripción:** Durante el desarrollo, guardar las salidas crudas de cada patrón de Fabric que se ejecuta en un archivo de log temporal (por ejemplo, en `target/fabric_outputs_temp.log` o un directorio de logs) para poder inspeccionar los resultados de cada análisis sin tener que esperar a que se cree la nota de Obsidian o en caso de que ocurra algún fallo intermedio.
*   **Plan:** Crear una utilidad sencilla que escriba las respuestas del LLM directamente en un archivo log plano inmediatamente después de cada ejecución exitosa de un patrón.

### 8. Optimización de patrones de Fabric para LLM local ⚡ (Para Mañana / Pendiente)
*   **Descripción:** Documentar o automatizar la optimización de los system prompts de patrones de Fabric pesados (como `extract_wisdom`) para el entorno local. Los system prompts originales exigen una cantidad inmensa de tokens generados, lo que demora más de 7 minutos en modelos locales como Qwen 35B.
*   **Plan:** 
    1. Recomendar/documentar la reducción de secciones redundantes de salida (ej. remover "Habits" o "Facts" si no se usan) en `~/.config/fabric/patterns/*/system.md`.
    2. Documentar la limitación del output en el prompt del sistema (ej. cambiar "Extract all ideas" por "Extract up to 10 key ideas").
    3. Analizar la viabilidad de proveer un set de "patrones livianos" optimizados para uso local dentro del proyecto.


