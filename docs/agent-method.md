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
| Instruções de projeto (`AGENTS.md`/`CLAUDE.md`) | Implementado | `zerith-core/instructions.rs` |
| Hooks de ciclo de vida | Implementado (ver `docs/hooks.md`) | `zerith-core/hooks.rs` |
| Skills (formato Agent Skills) | Implementado | `zerith-skills` |
| Subagentes e planos | Implementado em modo plano | `zerith-runtime/src/plan.rs` |
| Agentes nomeados em Markdown | Implementado | `zerith-core/agents.rs` |
| MCP (stdio e HTTP), ferramentas, recursos, prompts | Cliente implementado; ligado ao `-p` | `zerith-mcp` (`docs/mcp.md`) |
| Plugins em pacote | Manifesto e confiança implementados; carga de skills/agentes/hooks **planejada** | `zerith-plugins` |
| `/rewind` (checkpoints de arquivos e conversa) | Armazenamento e restauração implementados; **as ferramentas de arquivo ainda não chamam `record_file`** | `docs/checkpoints.md` |
| `--resume`, `--continue`, `--fork`, nome de sessão | Implementado | `zerith-cli` |
| Modo headless `-p` com `--output-format`, `--max-turns`, `--allowed-tools`, `--permission-mode`, `--json-schema` | Implementado (schema mínimo) | `zerith-cli` |
| `--max-budget-usd` | Implementado **com preços informados pelo usuário** (sem tabela própria) | `zerith-cli` |
| Memória entre sessões | Implementado (SQLite + FTS5), com pausa e exportação | `zerith-memory` |
| Compactação de contexto | Existe em `zerith-runtime` (`compact.rs`); critérios em constante evolução | |
| Rebobinar na janela | A janela já oferece "Rebobinar" quando há checkpoints | `zerith-ui` |

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
