# AGENTS.md — yt2action

Guía para agentes que trabajan en este repo. Qué es el proyecto y cómo se usa: `README.md`. Qué falta hacer: `TODO.md` y `2do.txt`.

## Reglas

- Rust, edición 2024. Errores con `anyhow`. Probar antes de diagnosticar: un bug se reproduce con un test antes de arreglarlo.
- `cargo test` tiene que pasar antes de cada commit.
- Commits con rutas explícitas, nunca `git add -A`: puede haber cambios de otra sesión en el árbol de trabajo.
- Lo que se da por hecho en `TODO.md` o `2do.txt` se probó de verdad; lo que no, se anota como "sin verificar".
- Un video con respuesta del LLM sin JSON es un **error**, no un éxito: no escribe nota ni marca el video como procesado (TODO #21).

## Checklist de cada release

Un release es subir `version` en `Cargo.toml` (no hay tags ni CI hoy). Antes de subirla:

1. **Revisar la última versión de Fabric.**
   ```bash
   gh api repos/danielmiessler/fabric/releases/latest --jq '.tag_name,.published_at'
   ./fabric --version        # el binario local, gitignored
   ```
   Si el local quedó atrás, bajar el release nuevo (el binario no va al repo) y anotar en el commit del release qué versión se probó.
2. **Sincronizar los patrones** con `yt2action sync-patterns --force` y probar con un video corto que los patrones que usamos (`summarize`, `extract_wisdom`) siguen dando una salida válida. Si un patrón cambió de formato, el parseo de la salida puede romperse.
3. **Mirar si hay patrones nuevos que valga incluir.** Comparar `ls ~/.config/fabric/patterns` contra `gh api repos/danielmiessler/fabric/contents/data/patterns`. No hay una métrica de popularidad en Fabric: el criterio es de juicio y se anota con su motivo. Candidatos hoy, a confirmar leyendo cada `system.md`:
   - `extract_recommendations` y `extract_insights`: producen acciones, que es lo que `yt2action` busca.
   - `youtube_summary` y `create_summary`: alternativas a `summarize` hechas para transcripciones.
   - `extract_ideas`, `extract_main_idea`, `analyze_claims`: útiles para charlas y paneles.
   No agregar un patrón sin probarlo con un video real y con el LLM local.
4. **Licencia.** Fabric es MIT (verificado el 2026-10-25). Si se copia un patrón dentro de este repo o de otro (por ejemplo un plugin de Lapacho), se incluye el aviso de copyright y la licencia de Fabric junto a la copia.
5. `cargo test`, subir `version` y commitear con mensaje narrativo.

## Cómo se prueba contra el LLM real

El LLM local es un servidor compatible con OpenAI (`/v1/chat/completions`, ver `~/.config/yt2action/config.toml`). Un video tarda minutos. `process <id> --stdout` imprime el resumen y no escribe nada: sirve para probar sin ensuciar el vault ni el estado.

## Relación con Lapacho

El plugin de ejemplo `youtube-summary` vive en el repo de Lapacho (`plugins/examples/`), no acá. No reutiliza `yt2action`: baja la transcripción y usa el patrón `summarize` de Fabric. Cualquier cambio en ese patrón se refleja allá a mano.
