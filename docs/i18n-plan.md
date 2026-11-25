# Internationalization (i18n) plan

Status: planned for the release after 0.4.9. Nothing here is implemented yet.

## Goal

Two separate decisions, following how Claude Code and opencode work:

1. **Model-facing text is English, always.** Tool descriptions, parameter
   docs, runtime notices appended to tool results, the system prompt, skill
   headers and token-saver prompts are written once, in English, and never
   translated. The model still answers in the language the person writes in.
2. **The graphical interface is multilingual.** The desktop app ships with
   `en` (default) and `pt-BR`, and adding a language means translating one
   file, with no code changes.

## Where text lives today

There is no language setting and no translation layer. Every string is a
literal in the code, mostly Portuguese, with a few English leftovers
(e.g. `no such tool` in `crates/zerith-tools/src/lib.rs`).

| Surface | Who reads it | Target | Location |
|---|---|---|---|
| Desktop UI (React 18, 43 `.ts/.tsx` files): ~320 labels/titles/placeholders/toasts, plus ~3,000 other quoted strings to triage (many are keys, CSS, IDs) | person | translated (`en`, `pt-BR`) | `crates/zerith-desktop/frontend/src` |
| Tauri command errors/status shown in the UI | person | translated, same catalog as the UI | `crates/zerith-desktop/src/commands.rs` |
| Tool descriptions and parameter docs (62 tools) | model | English only | `crates/zerith-tools/src/*.rs` |
| Runtime notices in tool results (`[saída completa: ...]`, `[o comando FALHOU]`, skill notes, reminders, ~100) | model | English only | `crates/zerith-runtime/src/{agent,pipeline,spill,compact,lib}.rs`, `crates/zerith-tools/src/*.rs` |
| System prompt, skill header, token-saver prompts | model | English only | `crates/zerith-core/src/lib.rs`, `crates/zerith-runtime/src/{lib,tokensaver}.rs` |
| CLI output and `--help` | person | English (no catalog for now) | `crates/zerith-cli/src` |
| User skills, plugins, workflows, memories, inventory | person/model | untouched: stay in the language they were written in | user data |
| Bundled skills and workflow templates | model | English | `bundled-skills/`, `plugins/`, templates |
| Docs | person | English as the main docs for the open-source project; `pt-BR` kept as a translation | `docs/` |

## Design

### 1. Model-facing text in English

- Rewrite tool descriptions, parameter docs, runtime notices, the system
  prompt, the skills header and the token-saver prompts in English. One
  commit per crate/area, no behavior change.
- Tool **names** and parameter **names** do not change (permissions,
  workflows, agent definitions and saved history reference them).
- The system prompt keeps "answer in the language the user writes in", so a
  person writing Portuguese still gets Portuguese replies.
- **Parsed notices**: some notices are read back by code: `exit N` as the
  first shell line (`verify::exit_of`, the token saver), `[saída completa:
  <path>` (`compact.rs`), frame lines kept by RTK (`tokensaver.rs`), skill
  markers in history (`agent.rs`). Inventory every parser first, switch each
  to the English marker and keep accepting the old Portuguese marker when
  reading, so sessions saved by older versions still compact and replay.
- **Prompt cache**: one fixed language means the system prompt and tool
  schemas are byte-stable across sessions, which helps cache hits.
- Measure before/after on `zbench3` (time, steps, tool choices, plan
  quality) to confirm no regression for Portuguese-speaking users.

### 2. Multilingual GUI

- `Settings.ui_language: Option<String>`, a BCP 47 tag (`en`, `pt-BR`).
  `None` means the OS UI language when shipped, else `en`. Fallback: exact
  tag, then base language (`pt-PT` to `pt-BR`), then `en`.
- Language selector in Settings; switching applies immediately, no restart.
- `i18next` + `react-i18next`, one JSON per locale in
  `frontend/src/locales/{en,pt-BR}.json`, namespaced by screen. Keys are
  semantic (`settings.language.label`), not the sentence.
- Tauri commands return a stable error code plus parameters; the frontend
  renders the message from the catalog. Logs and tests use the code.
- Dates and numbers via `Intl` with the selected locale.
- This setting does not touch anything the model reads.

### 3. CLI

English only for now. Revisit if users ask; the Tauri error codes from 2 can
be reused.

## Work plan

1. **Inventory**: script listing every literal per surface with file:line
   (Rust: `format!`, `ToolError::*`, notices; TSX: JSX text and
   `title/label/placeholder/toast`). It is the checklist for both tracks.
2. **Parsed notices**: English markers + backward-compatible readers, with
   tests, before moving any other model-facing text.
3. **Model-facing English**: tool schemas, notices, prompts, bundled skills;
   bench before/after.
4. **GUI i18n setup**: `ui_language`, i18next, selector, `en` and `pt-BR`
   files, Tauri error codes.
5. **GUI strings**: move screen by screen, one commit per screen.
6. **Docs**: English main docs, `pt-BR` translation; README screenshots.

## Checks

- CI: every key in `en.json` exists in `pt-BR.json` and vice versa; no unused
  keys.
- CI: no new user-facing literal in TSX outside the catalogs (lint over the
  inventory patterns).
- Test: sessions saved with the old Portuguese notices still compact and
  replay.
- Bench (`zbench3`): no regression after the model-facing switch, with the
  request written in Portuguese.

## Out of scope

- Translating model-facing text.
- Right-to-left layouts.
- Machine translation at runtime.
- Translating user data (session titles, memories, skills, inventory).

## References to study first

How Claude Code, opencode, Hermes and Codex word their tool descriptions and
notices, and how opencode's desktop/TUI handles UI language.
