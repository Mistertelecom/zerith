# Arquitetura

Um workspace Cargo na raiz do repositório. Sem tokio, sem reqwest: I/O bloqueante em threads, HTTP via
`ureq`, um banco SQLite.

## Crates e dependências

```text
zerith-cli ─┬─ zerith-runtime ─┬─ zerith-providers
            │                  ├─ zerith-sessions ── SQLite (rusqlite, bundled)
            │                  ├─ zerith-memory  ─── mesmo arquivo SQLite (FTS5)
            │                  └─ zerith-skills
            ├─ zerith-tools ── zerith-security
            ├─ zerith-mcp, zerith-plugins, zerith-importers
            └─ zerith-core  (usado por todos)
zerith-ui  ── Rune (git, rev bc05f817) + os mesmos crates de núcleo
```

| Crate | Responsabilidade |
| --- | --- |
| `zerith-core` | Identidade, `Paths`, prompt de sistema, configuração em camadas (`settings`), instruções de projeto (`instructions`), segredos no cofre do SO (`secrets`), runner de hooks (`hooks`) |
| `zerith-sessions` | Schema, migrações, sessões, mensagens, planos |
| `zerith-memory` | `MemoryBackend`, `SqliteMemory`, extração explícita |
| `zerith-providers` | Clientes OpenAI-compatible e Anthropic, SSE, `Cancel` |
| `zerith-runtime` | Um turno: histórico → prompt → provider → mensagem gravada; planos |
| `zerith-tools` | Registry de ferramentas com permissão, timeout e política |
| `zerith-security` | Presets de sandbox, `Verdict`, redação de segredos |
| `zerith-skills` | Formato Agent Skills, catálogo, instalação com rollback |
| `zerith-mcp` | Cliente MCP (JSON-RPC 2.0 sobre stdio) |
| `zerith-plugins` | Manifestos, ativação, revisão explícita |
| `zerith-importers` | Import read-only do Hermes |
| `zerith-cli` | Binário `zerith` |
| `zerith-ui` | Janela nativa em Rune |

## Fluxo de um turno

1. O host (CLI ou janela) resolve provider e modelo (env, depois
   `settings.toml`, depois chave no cofre do SO).
2. `zerith-runtime` grava a mensagem do usuário, monta o prompt fixo
   (`zerith_core::system_prompt`), anexa memória e skills **depois** dele
   (preserva o prefix cache do provider) e replays até `HISTORY_LIMIT`
   mensagens.
3. O provider faz streaming; deltas vão ao host por `RunEvent`.
4. A resposta inteira é gravada; nada parcial é persistido.

## Configuração

Ver [docs/settings.md](docs/settings.md). Camadas: padrão < usuário <
projeto < projeto-local < env.

## O que existe só como biblioteca (planejado no runtime)

- `zerith_core::hooks::run_hooks` — o runtime ainda **não** chama hooks.
- `Settings::permissions` (`allow`/`ask`/`deny`) — `zerith-tools` ainda usa
  só o preset de `zerith-security`; as regras ainda não são aplicadas.
- `system_prompt_with_instructions` — o runtime ainda usa `system_prompt`.
- `Profile::limits()` — os números existem; `run_plan` e o histórico ainda
  usam constantes próprias.
