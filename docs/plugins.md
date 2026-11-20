# Plugins

Um plugin é uma pasta com `plugin.json`. Pode ser um processo MCP próprio
(`command`) e/ou um **pacote** de skills, agentes, hooks e servidores MCP, como
os pacotes de plugin do Claude Code. Código em `zerith-plugins`.

```json
{
  "name": "revisao",
  "version": "1.0.0",
  "description": "Revisão de código",
  "skills": ["skills/revisar"],
  "agents": ["agents/revisor.md"],
  "hooks": [{ "event": "SessionStart", "command": "echo pronto" }],
  "mcp": [{ "name": "docs", "command": "docs-mcp", "args": ["--stdio"] }]
}
```

- `skills`: pastas com `SKILL.md`. `agents`: arquivos `.md`. Caminhos relativos,
  sem `..`, dentro do plugin, e **precisam existir** — a instalação recusa o
  resto (um `../fora` não passa).
- `hooks`: comandos de shell; `mcp`: mesmo formato de `[[mcp.servers]]`, o
  servidor aparece como `<plugin>-<nome>`.
- `command` é opcional para um pacote sem processo próprio.

## Loja (formato do Claude Code)

Plugins do Claude Code instalam sem mudar nada no código do Zerith. A loja
`zerith` vem embutida (catálogo em `.claude-plugin/marketplace.json` na raiz
do repositório, lido também sem internet):

```bash
zerith plugin browse                          # o que as lojas oferecem
zerith plugin install claude-bughunter@zerith # baixa só a pasta do plugin e liga
zerith plugin marketplace add elementalsouls/Claude-BugHunter   # outra loja
zerith plugin install claude-bughunter@elementalsouls
zerith plugin marketplace list | update <nome> | remove <nome>
```

Na janela: **Ajustes › Skills › Plugins** (botões Instalar, Adicionar loja,
liga/desliga, remover).

O que vira o quê na instalação:

| Claude Code | Zerith |
| --- | --- |
| `skills/<nome>/SKILL.md` | skill (a IA carrega quando o assunto aparece) |
| `commands/*.md` (`/hunt`) | skill `<plugin>-<comando>` em `.zerith-commands/` |
| `agents/*.md` | agente da camada `plugin` |
| `mcpServers` / `.mcp.json` | servidor MCP `<plugin>-<nome>` (`${CLAUDE_PLUGIN_ROOT}` resolvido) |
| `hooks` | não convertidos (contrato de stdin e nomes de ferramenta do Claude Code) |

Instalado pela loja = **ativo**: skills e agentes valem na próxima mensagem.
O que roda programa (MCP, hooks) só liga depois de `zerith plugin trust <nome> --yes`
(na janela: ícone de alerta na linha do plugin).

## Confiança

Instalar **não** é confiar. De uma pasta (`zerith plugin install ./x`) o plugin entra `disabled` + não revisado; da loja entra ativo, mas os programas dele (MCP, hooks) continuam bloqueados até `trust`.

| Estado | Efeito |
| --- | --- |
| instalado | só metadados |
| `enable` | skills e agentes (texto que a IA lê) |
| `enable` + `trust` | também hooks e MCP (programas) |
| `untrust` | hooks e MCP saem; skills e agentes ficam |
| `disable` | tudo é retirado |

`trust` mostra o que o plugin executa (comando, hooks, servidores) e exige
`--yes`. Nada disto é sandbox: o plugin roda com os seus privilégios.

## CLI

```bash
zerith plugin install ./revisao       # copia a pasta (symlinks são ignorados)
zerith plugin list
zerith plugin show revisao            # o que ele pede
zerith plugin enable revisao
zerith plugin trust revisao --yes
zerith plugin untrust revisao
zerith plugin disable revisao
zerith plugin remove revisao
zerith plugin tools revisao           # só se tiver `command`
```

`zerith plugins …` continua aceito.

## O que é carregado pelos hosts

`Library::contributions()` entrega as contribuições apenas de plugins
habilitados e revisados (`enabled` + `trusted`), por meio de `Contributions`:

- `skill_dirs()` — pastas com `SKILL.md` empacotadas no plugin;
- `agent_defs()` — definições `.md` carregadas com camada `"plugin"`;
- `apply_hooks(&mut settings)` — anexa os hooks declarados ao `Settings` do turno;
- `mcp` — servidores `ServerConfig` nomeados `<plugin>-<nome>`.

Tanto o CLI (`zerith -p`, `zerith chat`, `zerith agents list`) quanto a janela
(`zerith-desktop`, no envio do chat e na tela **Agentes**) carregam skills, agentes,
hooks e servidores MCP de plugins habilitados e revisados.

Precedência em colisão de nomes: servidores MCP do `settings.toml` / `mcp.toml`
vencem os de plugin; agentes das camadas `user` e `project` têm precedência
sobre agentes de camada `plugin`.
