# Referências

O Zerith é um projeto próprio. Os projetos abaixo foram estudados; a coluna
"O que veio" diz exatamente o que cada um deixou aqui.

| Projeto | Licença | O que veio | Revisão consultada |
| --- | --- | --- | --- |
| OpenHuman | GPL-3.0-only | **Referência de produto e design.** Ideia de harness desktop local; paleta, escala de tipos e geometria da janela reproduzidas como tokens de design. Nenhum crate do OpenHuman é dependência | `5e6d98d433` |
| Tauri 2 | MIT / Apache-2.0 | **Dependência** (`zerith-desktop`), casca desktop nativa para o frontend React/TypeScript sobre o núcleo Rust | `2.x` |
| Claude Code | Proprietária | **Só conceitos** (hooks, camadas de settings, regras `Tool(pattern)`, modo `-p`). Nenhum código copiado | — |
| DeepSeek Harness | MIT | Conceitos de arquitetura reimplementados em Rust. Nenhum código copiado | — |
| Hermes Agent | MIT | Importador read-only do banco `~/.hermes/state.db` (schema lido, não código) | schema verificado na rev `4162ecf1` |
| 9Router | MIT | **Só conceitos** (gateway multi-conta por prioridade, combos, cooldown e failover) e importador opcional de credenciais OAuth. A dependência de runtime externa (`localhost:20128/v1`) foi removida e substituída pelo gateway nativo em Rust | — |
| Agent Skills (spec) | Apache-2.0 | Formato `SKILL.md` adotado | — |
