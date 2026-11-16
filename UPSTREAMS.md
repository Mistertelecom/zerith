# Referências

O Zerith é um projeto próprio. Os projetos abaixo foram estudados; a coluna
"O que veio" diz exatamente o que cada um deixou aqui.

| Projeto | Licença | O que veio | Revisão consultada |
| --- | --- | --- | --- |
| OpenHuman | GPL-3.0-only | **Referência de produto e design.** Ideia de harness desktop local; paleta, escala de tipos e geometria da janela (sidebar 240 pt, folha com raio 16) reproduzidas como tokens em `crates/zerith-ui/src/ui.rs`. Nenhum crate ou arquivo do OpenHuman é dependência ou foi copiado | `5e6d98d433` |
| Rune | MIT | **Dependência** (`zerith-ui`, git pinado) + 5 crates corrigidos em `vendor/`, copiados de `rune/vendor` | `bc05f817ed19488a10a6f1a928cc0521c1c70c75` |
| Claude Code | Proprietária | **Só conceitos** (hooks, camadas de settings, regras `Tool(pattern)`, modo `-p`). Nenhum código copiado | — |
| DeepSeek Harness | MIT | Conceitos de arquitetura reimplementados em Rust. Nenhum código copiado | — |
| Hermes Agent | MIT | Importador read-only do banco `~/.hermes/state.db` (schema lido, não código) | schema verificado na rev `4162ecf1` |
| 9Router | MIT | Integração só por API HTTP OpenAI-compatible (`localhost:20128/v1`) | — |
| Agent Skills (spec) | Apache-2.0 | Formato `SKILL.md` adotado | — |

## Atualizar

- **Rune:** trocar a `rev` em `crates/zerith-ui/Cargo.toml` (as duas linhas)
  e recopiar `vendor/` com os scripts `vendor-*.sh` do checkout do Rune na
  mesma revisão.
