# Internationalization (i18n) plan

Status: planned for the release after 0.4.9. Nothing here is implemented yet.

## Goal

Let a person pick the language Zerith speaks: the desktop app, the CLI, the
notices the runtime adds to tool results, and the default language of the
model's replies. Ship with `pt-BR` (current text, the source of truth) and `en`.
Adding a third language must be "translate one file per surface", with no code
changes.

## Where text lives today

There is no language setting and no translation layer. Every string is a
literal in the code, mostly Portuguese, with a few English leftovers
(e.g. `no such tool` in `crates/zerith-tools/src/lib.rs`).

| Surface | Who reads it | Rough size | Location |
|---|---|---|---|
| Desktop UI (React 18, 43 `.ts/.tsx` files) | person | ~320 labels/titles/placeholders/toasts + ~3,000 other quoted strings to triage (many are keys, CSS, IDs) | `crates/zerith-desktop/frontend/src` |
| Tauri commands' error/status text shown in the UI | person | dozens | `crates/zerith-desktop/src/commands.rs` |
| CLI output and `--help` | person | hundreds | `crates/zerith-cli/src` (clap `args.rs`, `cmds/`) |
| Tool descriptions and parameter docs (schemas) | model | 62 tools | `crates/zerith-tools/src/*.rs` |
| Runtime notices appended to tool results (`[saída completa: ...]`, `[o comando FALHOU]`, skill notes, reminders) | model | ~100 | `crates/zerith-runtime/src/{agent,pipeline,spill,compact,lib}.rs`, `crates/zerith-tools/src/*.rs` |
| System prompt, skill header, token-saver prompts | model | a few long blocks | `crates/zerith-core/src/lib.rs`, `crates/zerith-runtime/src/{lib,tokensaver}.rs` |
| Bundled skills, plugins, workflow templates | model / person | per file | `bundled-skills/`, `plugins/`, workflow templates |
| Docs | person | 26 files | `docs/` |

## Design

### 1. Setting

- `Settings.language: Option<String>`, a BCP 47 tag (`pt-BR`, `en`). Env
  override `ZERITH_LANG`.
- `None` means auto: the OS UI language when it matches a shipped locale,
  else `en`. Lookup falls back from the exact tag to the base language
  (`pt-PT` to `pt`), then to `pt-BR`.
- Desktop: a selector in Settings that switches the language without a
  restart. CLI: `zerith config set language en`.

### 2. Person-facing text (UI, CLI, Tauri messages)

- **Desktop**: `i18next` + `react-i18next` (already standard, no runtime cost
  worth measuring). One JSON file per locale in
  `frontend/src/locales/{pt-BR,en}.json`, namespaced by screen. Keys are
  semantic (`settings.language.label`), not the Portuguese sentence.
- **Rust (CLI, Tauri commands)**: a tiny in-house `t!("key", arg = value)`
  backed by TOML files embedded with `include_str!`
  (`crates/zerith-core/locales/{pt-BR,en}.toml`). Evaluate `rust-i18n` first;
  take it only if it stays small and does not pull in a heavy dependency tree.
  Plural and number/date formatting: only where it is actually needed (counts
  in notices); no ICU.
- Errors keep a stable machine code alongside the translated text, so logs,
  tests and support stay language-independent.

### 3. Model-facing text (tool schemas, runtime notices, prompts)

The model understands any language equally well, so the goal here is
consistency with the person's language, not comprehension. Rules:

- Tool **names** and parameter **names** never change (they are API,
  referenced by permissions, workflows, agent definitions and saved history).
- Tool descriptions, parameter docs and runtime notices go through the same
  Rust catalog, selected by `language`.
- **Prompt cache**: the system prompt and tool schemas must be byte-stable
  inside a session. Resolve the language once at session start and keep it
  for the whole session; changing the setting applies to new sessions only.
- The system prompt keeps "answer in the language the user writes in". The
  setting only picks the default when the person's message gives no signal
  (e.g. a pasted log or command).
- Notices that other code parses (`exit N` as the first line, read by
  `verify::exit_of` and the token saver; `[saída completa: <path>` read by
  `compact.rs`; frame lines kept by RTK in `tokensaver.rs`) get a
  language-independent marker so parsing does not depend on the translation.
  Inventory these parsers before translating any notice.

### 4. Skills, plugins, workflows

- Skills stay in whatever language they were written; the loader does not
  translate them. Optional `SKILL.<lang>.md` next to `SKILL.md` is picked
  when it exists.
- Bundled skills and workflow templates: ship `pt-BR` and `en` variants only
  for the bundled set.

### 5. Docs

- Keep `docs/` in Portuguese as today; add `docs/en/` for the pages a new
  user needs first (README, install, providers, settings, permissions,
  troubleshooting). Translate the rest on demand.

## Work plan

1. **Inventory**: script that lists every user-facing literal per surface
   (Rust: `format!`, `ToolError::*`, notices; TSX: JSX text and
   `title/label/placeholder/toast`), with file:line. Output feeds the key
   files and is the checklist.
2. **Setting + Rust catalog**: `Settings.language`, resolution, `t!`, the
   two TOML files, unit tests for fallback and missing keys.
3. **Parsed notices first**: add stable markers to the notices other code
   parses, with tests, before any text moves.
4. **Desktop UI**: i18next setup, language selector, move strings screen by
   screen (one commit per screen).
5. **CLI and Tauri messages**.
6. **Model-facing text**: tool descriptions, notices, prompts; session-level
   language pinning; prompt-cache test (same session, same bytes).
7. **Skills/workflows variants, docs/en**.
8. **Release**: `en` screenshots in README, changelog.

## Checks

- CI test: every key in `pt-BR` exists in `en` and vice versa (UI JSON and
  Rust TOML); no unused keys.
- CI test: no new user-facing literal outside the catalogs in the touched
  surfaces (lint over the inventory patterns).
- Prompt-cache test: two turns in one session produce identical system
  prompt and tool schema bytes, whatever the setting does mid-session.
- Bench (`zbench3`) run in `pt-BR` and `en`: same tool choices, same plan
  quality, no regression in time or steps.

## Out of scope

- Right-to-left layouts.
- Machine translation at runtime.
- Translating user data (session titles, memories, inventory comments).

## References to study first

How opencode, Hermes, Codex and Claude Code handle UI language, model-facing
text and prompt-cache stability, before choosing the library and the catalog
format.
