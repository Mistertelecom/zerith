# MCP no laço do agente

Servidores MCP configurados viram ferramentas do agente. Código em
`zerith-mcp` (`config.rs`, `tools.rs`).

## Configuração

```toml
# settings.toml (usuário) — ou <raiz>/mcp.toml, que o CLI edita
[[mcp.servers]]
name = "docs"
command = "npx"
args = ["-y", "@exemplo/servidor-docs"]

[[mcp.servers]]
name = "web"
url = "https://exemplo.test/mcp"     # Streamable HTTP
timeout_ms = 20000                   # opcional; padrão 30 s
```

`name` aceita letras, números, `-` e `_`. Use `command` **ou** `url`.
Camadas lidas: `settings.toml` e `mcp.toml` do usuário; do projeto,
`.zerith/settings.toml`, `settings.local.toml` e `mcp.toml`. Quem define o
mesmo nome por último vence. Arquivo inválido é relatado e o resto carrega.

**Servidor definido pelo projeto não roda sozinho.** Um repositório poderia
declarar `command = "qualquer coisa"`. Ele fica pendente até você rodar
`zerith mcp approve <nome> --yes`, que só vale se gravado no arquivo **do
usuário**; um projeto não consegue se aprovar.

## Ferramentas

Cada ferramenta do servidor vira `mcp__<servidor>__<ferramenta>` (caracteres
fora de `[A-Za-z0-9_-]` viram `_`, limite de 64) com o schema JSON do servidor.
Uma colisão após a limpeza é recusada com mensagem, não sobrescreve.

- **Preguiçoso e persistente.** Nada é iniciado ao montar o registro.
  `McpRegistry::register_into(&mut Registry)` conecta cada servidor uma vez
  (os nomes só existem depois de `tools/list`); a conexão fica viva e é
  compartilhada. Chamadas são serializadas por servidor.
- Se o servidor cai ou estoura o tempo, a conexão é descartada e a **próxima**
  chamada reconecta.
- Filhos stdio morrem quando o registro é solto (`Client::drop`).
- Falha em um servidor não impede os outros; os erros voltam em
  `Registered::errors`.
- `isError: true` do servidor vira erro de ferramenta: o modelo é avisado.

**Permissão.** Toda chamada MCP é uma ação `Network` para a política, com o
nome da ferramenta como alvo das regras:
`ask = ["mcp__github__*"]`, `deny = ["mcp__docs__apagar"]`. Nada aqui decide
sozinho se uma chamada pode rodar.

**Limite honesto.** `Client::request` bloqueia até a resposta ou o próprio
tempo limite; um cancelamento do turno só é visto entre chamadas, por isso as
ferramentas MCP aparecem como `cancellable: false`.

## Recursos e prompts

- `@servidor:recurso` no texto do usuário: `parse_mentions` acha os tokens (só
  depois de espaço, então e-mails não casam) e
  `McpRegistry::expand_mentions(texto)` anexa um bloco `<recurso …>` por
  menção resolvida. Aceita URI (`doc://guia`) ou o nome de `resources/list`.
  Falha é relatada e a menção permanece no texto. Servidor desconhecido é
  ignorado (provavelmente não era menção). Corte em 100 000 caracteres; blob
  binário é nomeado, não inserido.
- Prompts: `McpRegistry::prompt_commands()` lista como `/mcp__<servidor>__<prompt>`
  com argumentos e `get_prompt(servidor, prompt, args)` expande para texto.

**Estado real:** `zerith -p`, `zerith chat` e a janela (`zerith-ui`) carregam o
`McpRegistry` (incluindo servidores de plugins habilitados e revisados),
conectam e registram as ferramentas com `register_into` e expandem menções
`@servidor:recurso` no texto enviado. Os helpers `prompt_commands()` e
`get_prompt()` seguem disponíveis em `McpRegistry` para expansão de prompts MCP.

## CLI

```bash
zerith mcp list
zerith mcp add docs -- npx -y @exemplo/servidor-docs
zerith mcp add web --url https://exemplo.test/mcp --timeout-ms 20000
zerith mcp remove docs                 # só entradas de <raiz>/mcp.toml
zerith mcp approve docs --yes          # servidor vindo do projeto
zerith mcp tools docs
zerith mcp call docs buscar '{"q":"x"}'
zerith mcp tools npx -y @exemplo/servidor-docs     # servidor avulso, sem config
```
