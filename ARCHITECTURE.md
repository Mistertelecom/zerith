# Arquitetura

Um workspace Cargo na raiz do repositório. O núcleo (CLI, runtime, providers,
memória, sessões, MCP) usa I/O bloqueante em threads, HTTP via `ureq` e um único
banco SQLite; a camada desktop (`zerith-desktop`) usa Tauri 2 + React/TypeScript
chamando o mesmo núcleo Rust.

## Crates e dependências

```text
zerith-cli ─────┬─ zerith-runtime ─┬─ zerith-providers
                │                  ├─ zerith-sessions ── SQLite (rusqlite, bundled)
                │                  ├─ zerith-memory  ─── mesmo arquivo SQLite (FTS5)
                │                  └─ zerith-skills
                ├─ zerith-tools ── zerith-security
                ├─ zerith-mcp, zerith-plugins, zerith-importers
                └─ zerith-core  (usado por todos)
zerith-desktop ── Tauri 2 + React/TypeScript + os mesmos crates de núcleo
```

| Crate | Responsabilidade |
| --- | --- |
| `zerith-core` | Identidade, `Paths`, prompt de sistema, configuração em camadas (`settings`, incluindo `[[accounts]]`, `[routing]`, `[token_saver]`), instruções de projeto (`instructions`), agentes em Markdown (`agents`), segredos no cofre do SO (`secrets`), runner de hooks (`hooks`) |
| `zerith-sessions` | Schema, migrações (até `V7` com `usage_records` e `account_cooldowns`), sessões, mensagens, planos, checkpoints e estatísticas de cota/economia |
| `zerith-memory` | `MemoryBackend`, `SqliteMemory`, extração explícita |
| `zerith-providers` | Clientes OpenAI-compatible e Anthropic, presets nativos (`ProviderKind::PRESETS`), `ModelSpec`, SSE, `Cancel` e `Gateway` multi-conta com failover antes do primeiro token |
| `zerith-runtime` | Um turno: histórico → gateway nativo → Token Saver (`ponytail`, `caveman`, `rtk`) → provider → ferramentas → mensagem gravada e contabilizada; delegação multi-modelo de subagentes, planos, compactação, hooks e checkpoints |
| `zerith-tools` | Registry de ferramentas com permissão, timeout, política e `Host` (`before_write`) |
| `zerith-security` | Presets de sandbox, modos, regras (`allow`/`ask`/`deny`), `Verdict`, redação de segredos |
| `zerith-skills` | Formato Agent Skills, catálogo, instalação com rollback |
| `zerith-mcp` | Cliente MCP (JSON-RPC 2.0 sobre stdio e Streamable HTTP) e registro no laço |
| `zerith-plugins` | Manifestos, ativação, revisão explícita e contribuições de pacote (`skills`, `agents`, `hooks`, `mcp`) |
| `zerith-importers` | Import read-only do Hermes (SQLite e JSONL), pré-visualização e retomada incremental por mensagem |
| `zerith-cli` | Binário `zerith` |
| `zerith-desktop` | Janela desktop em Tauri 2 + frontend React/TypeScript (e auto-updater com assinatura Ed25519) |

## Fluxo de um turno

1. O host (CLI `-p`/`chat` ou janela `zerith-desktop`) resolve provider/contas e
   modelo (`--model`, `ZERITH_MODEL`, `[routing] main_model`, `model`,
   `[[accounts]]`), carrega as contribuições de plugins habilitados e
   revisados (`skills`, `agents`, `hooks`, `mcp`) e conecta o `McpRegistry`.
2. `zerith-runtime` dispara `SessionStart` (na 1ª mensagem da sessão) e
   `UserPromptSubmit` (que pode bloquear ou injetar contexto), abre o
   checkpoint do turno (`Checkpoint::begin` ligado a `write_file`/`edit_file`
   via `Host::on_before_write`), grava a mensagem do usuário, monta o prompt
   fixo (`zerith_core::system_prompt_with_instructions`), anexa instruções do
   agente, instruções do Token Saver (`Ponytail` + `Caveman`), memória e
   skills **depois** dele (preservando o prefix cache do provider) e reenvia
   até `Profile::limits().history_window` mensagens.
3. O gateway nativo (`resolve_gateway` / `zerith-providers::Gateway`) filtra
   contas em cooldown (`account_cooldowns`) ou com cota esgotada
   (`usage_records`), seleciona a conta elegível por prioridade (ou modelo
   delegado de subagente via `RoutingSettings::resolve_subagent_model`) e faz
   streaming com failover automático em falhas antes do primeiro token; deltas
   vão ao host por `RunEvent` (incluindo `RunEvent::Route` e
   `RunEvent::TokenSaved`). Saídas grandes ou verbosas de ferramentas passam
   pelo filtro determinístico `RTK` (`tokensaver.rs`) antes de voltar ao modelo.
   Chamadas de ferramenta passam pelo pipeline e pelo middleware `Guards`
   (regras de permissão, `PreToolUse`, `PermissionRequest`, aprovação
   interativa via `ApprovalGate`, `PostToolUse`/`PostToolUseFailure`), com
   `PreCompact` antes da compactação por pressão de contexto e `Stop` ao
   encerrar sem chamadas de ferramenta.
4. Ao final de cada chamada de modelo, o consumo e os tokens economizados por
   protocolo (`saved_rtk_tokens`, `saved_ponytail_tokens`,
   `saved_caveman_tokens`) são persistidos em `usage_records`; a resposta
   inteira é gravada; nada parcial é persistido. Nos hosts CLI (`headless` e
   `chat`), `SessionEnd` roda ao encerrar.

## Interface desktop (`zerith-desktop`)

A janela em Tauri 2 + React/TypeScript renderiza o transcript em Markdown (AST
segura sem injeção de HTML), diffs inline por arquivo com confirmação ao desfazer,
edição e ramificação (fork) de conversa, pasta de workspace por conversa, anexos
de imagem e mídia inline, comandos de barra no composer, painel de **Contas /
Provedores & Gateway Local** (OAuth, presets nativos, múltiplas contas, delegação
`[routing]` e **Cotas & Uso**), gerenciamento de **MCP**, **Skills**, **Agentes**,
**Memória**, seção **Token Saver**, **Importar Hermes** e atualização automática
com verificação obrigatória de SHA-256 + assinatura Ed25519.

## Configuração

Ver [docs/settings.md](docs/settings.md). Camadas: padrão < usuário <
projeto < projeto-local < env.

## Limites e lacunas em aberto

- **Confirmação de hooks de projeto:** hooks de camada de projeto já rodam
  durante o ciclo do turno com os privilégios do usuário; confirmação explícita
  antes de executá-los permanece planejada ([docs/hooks.md](docs/hooks.md)).
- **`Profile::limits().cache_entries`:** o valor é definido por perfil, mas
  nenhum cache em memória o lê ainda (`history_window` e `max_subagents` já
  são aplicados pelo runtime e pelo `plan run`).
- **Sandbox e `shell`:** a política é checada no processo; não há jaula de SO
  completa em todas as plataformas ([docs/sandbox.md](docs/sandbox.md)), e
  edições feitas por `shell` ficam fora do rastreamento de checkpoints.
- **`tools call` e `policy` no CLI:** hoje todos os fluxos principais (`-p`,
  `chat`, `tools call`, `policy` e `zerith-desktop`) usam `Policy::from_settings`.
