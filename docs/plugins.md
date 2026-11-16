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

## Confiança

Instalar **não** é confiar. O plugin entra `disabled` + não revisado.

| Estado | Efeito |
| --- | --- |
| instalado | só metadados |
| `enable` | ainda nada carrega |
| `enable` + `trust` | `Library::contributions()` devolve skills, agentes, hooks e MCP |
| `untrust` / `disable` | tudo é retirado de novo |

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
(`zerith-ui`, no envio do chat e na tela **Agentes**) carregam skills, agentes,
hooks e servidores MCP de plugins habilitados e revisados.

Precedência em colisão de nomes: servidores MCP do `settings.toml` / `mcp.toml`
vencem os de plugin; agentes das camadas `user` e `project` têm precedência
sobre agentes de camada `plugin`.
