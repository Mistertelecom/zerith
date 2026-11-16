# Upstreams e referências

| Projeto | Licença | Relação | Revisão |
| --- | --- | --- | --- |
| OpenHuman | GPL-3.0-only | **Fork.** Histórico git preservado; remoto `upstream` | HEAD `5e6d98d433` |
| Rune | MIT | **Dependência** (`zerith-ui`, git pinado) + 5 crates vendorizados em `vendor/` copiados de `rune/vendor` | `bc05f817ed19488a10a6f1a928cc0521c1c70c75` |
| Claude Code | Proprietária | **Só conceitos** (hooks, camadas de settings, regras `Tool(pattern)`, modo `-p`). Nenhum código copiado | — |
| DeepSeek Harness | MIT | Conceitos de arquitetura reimplementados em Rust. Nenhum código copiado | — |
| Hermes Agent | MIT | Importador read-only do banco `~/.hermes/state.db` (schema lido, não código) | schema verificado na rev `4162ecf1` |
| 9Router | MIT | Integração só por API HTTP OpenAI-compatible (`localhost:20128/v1`) | — |
| Agent Skills (spec) | Apache-2.0 | Formato `SKILL.md` adotado | — |

## Atualizar

- **OpenHuman:** `git fetch upstream` na raiz; o workspace `zerith/` não
  depende de crates do OpenHuman, então merges de upstream não quebram o build
  daqui.
- **Rune:** trocar a `rev` em `crates/zerith-ui/Cargo.toml` (as duas linhas)
  e recopiar `vendor/` com os scripts `vendor-*.sh` do checkout do Rune na
  mesma revisão.

Auditoria completa: `../docs/audit/initial-audit.md` (§2, §9).
