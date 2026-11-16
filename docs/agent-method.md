# Método de agente: Claude Code e DeepSeek Harness

O Zerith estudou o método de trabalho do Claude Code e a arquitetura do
DeepSeek Harness e **reimplementou conceitos** em Rust. Nada foi copiado: sem
código, prompts ou texto desses projetos. Este mapa diz o que existe e o que é
só intenção. "Implementado" significa que há código e teste neste repositório;
não significa paridade de comportamento.

## Conceitos do Claude Code

| Conceito | Estado no Zerith | Onde |
| --- | --- | --- |
| Laço ferramenta → resultado → próximo passo | Implementado | `zerith-runtime/src/agent.rs` |
| Aprovação humana por chamada, com "permitir na sessão" | Implementado | `ApprovalGate` |
| Modos de permissão (`default`, `acceptEdits`, `plan`, `dontAsk`, `bypassPermissions`) | Implementado | `zerith-security` |
| Regras `allow`/`ask`/`deny` por ferramenta e padrão | Implementado | `Policy::check_tool` |
| Sandbox de SO | **Parcial**: política checada pelo Zerith; `zerith doctor` mostra o nível | `zerith-security` |
| Instruções de projeto (`AGENTS.md`/`CLAUDE.md`) | Implementado (`system_prompt_with_instructions`) | `zerith-core/instructions.rs` |
| Hooks de ciclo de vida | Implementado para os 9 eventos (`Guards` + runtime + hosts CLI/UI; confirmação explícita para hooks de projeto planejada) | `zerith-core/hooks.rs`, `zerith-runtime` (`docs/hooks.md`) |
| Skills (formato Agent Skills) | Implementado | `zerith-skills` |
| Subagentes e planos | Implementado em modo plano | `zerith-runtime/src/plan.rs` |
| Agentes nomeados em Markdown | Implementado | `zerith-core/agents.rs` |
| MCP (stdio e HTTP), ferramentas, recursos, prompts | Cliente e registro implementados; ligado a `-p`, `chat` e `zerith-ui` (incluindo servidores de plugins e `@servidor:recurso`) | `zerith-mcp` (`docs/mcp.md`) |
| Plugins em pacote | Implementado: manifesto, confiança (`enable` + `trust`) e carga de skills, agentes, hooks e MCP via `contributions()` no CLI e na UI | `zerith-plugins` (`docs/plugins.md`) |
| `/rewind` (checkpoints de arquivos e conversa) | Implementado: runtime abre checkpoint por turno e `write_file`/`edit_file` chamam `before_write` → `record_file` (`shell` fica fora do rastreamento) | `docs/checkpoints.md` |
| `--resume`, `--continue`, `--fork`, nome de sessão | Implementado | `zerith-cli` |
| Modo headless `-p` com `--output-format`, `--max-turns`, `--allowed-tools`, `--permission-mode`, `--json-schema` | Implementado (schema mínimo) | `zerith-cli` |
| `--max-budget-usd` | Implementado **com preços informados pelo usuário** (sem tabela própria) | `zerith-cli` |
| Memória entre sessões | Implementado (SQLite + FTS5), com pausa e exportação | `zerith-memory` |
| Compactação de contexto | Existe em `zerith-runtime` (`compact.rs`); critérios em constante evolução | |
| Rebobinar e interface na janela | Implementado em Rune: botão `Rebobinar` com resumo de arquivos/linhas alteradas do turno, `/rewind` e `/mode`, números de linha reais no diff inline, preview em andamento de ferramentas em execução e borda semântica de modo no composer | `zerith-ui` |
| Gateway nativo multi-conta, cotas e delegação entre modelos | Implementado: `[[accounts]]`, cooldown + failover antes do primeiro token, `usage_records`/`account_cooldowns` (`V7`), `[routing]` (`main_model` + `subagent_model`/`explore_model`/`plan_model`/`role_models` + combos), CLI `usage`/`quota`/`accounts`/`routing` e tela **Provedores & Gateway Local** | `zerith-providers`, `zerith-sessions`, `zerith-runtime`, `zerith-cli`, `zerith-ui` |
| Token Saver (`Ponytail`, `Caveman`, `RTK`) | Implementado com métricas individuais por protocolo (`saved_rtk_tokens`, `saved_ponytail_tokens`, `saved_caveman_tokens`) em `tokensaver.rs`, `usage_records`, CLI `tokensaver`/`usage` e **Configurações** | `zerith-runtime`, `zerith-sessions`, `zerith-cli`, `zerith-ui` |

## Conceitos do DeepSeek Harness

| Conceito | Estado no Zerith |
| --- | --- |
| Trilha de eventos por turno (início, passos, ferramentas, fim) para auditoria e detecção de queda | Implementado: tabela `events` (migração v4) |
| Pipeline de middlewares em volta das ferramentas | Implementado em `zerith-runtime` (`pipeline.rs`, `guards.rs`) |
| Chamadas de ferramenta seguras em paralelo | Implementado: só ferramentas que se declaram seguras |
| Saída grande derramada para arquivo em vez de truncada | Implementado (`spill.rs`) |
| Perfis de execução (janela de histórico, subagentes) | Implementado (`Profile`) |
| Caixa de entrada entre agentes | **Planejado** |
| Orquestração distribuída | **Planejado**; fora do escopo do binário local |

## Diferenças deliberadas

- **Sem async e sem `tokio`/`reqwest`.** Threads e `ureq`. Mais simples de
  auditar, ao custo de leituras bloqueantes (cancelamento só entre pedaços).
- **Um arquivo SQLite** para sessões, memória, eventos e checkpoints.
- **Plugins e servidores de projeto são não confiáveis por padrão.**
- **Honestidade sobre limites:** o que não é imposto pelo SO é dito assim.

Os números de aceite estão em `docs/aceite.md`, recalculados a partir da
tabela e não de memória.
