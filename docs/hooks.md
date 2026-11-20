# Hooks

Comandos do usuário executados em pontos fixos da sessão. Contrato
inspirado no do Claude Code (só o conceito; nenhum código copiado).

**Estado:** todos os 9 eventos estão ligados.
`PreToolUse`, `PermissionRequest`, `PostToolUse` e `PostToolUseFailure` rodam
por chamada de ferramenta via `zerith_runtime::guards::Guards` (middleware em
`AgentOptions.middleware`). No turno (`zerith-runtime/src/agent.rs`), quando
`TurnContext.settings` está presente, o runtime dispara `SessionStart` (na 1ª
mensagem da sessão), `UserPromptSubmit` (antes de gravar o prompt),
`PreCompact` (antes de sumarizar) e `Stop` (quando o modelo termina sem chamadas
de ferramenta). `SessionEnd` é disparado ao encerrar pelos hosts CLI
(`headless` e `chat`). Os hosts (`zerith -p`, `zerith chat` e `zerith-desktop`)
também aplicam os hooks de plugins habilitados e revisados no `Settings` do
turno.

## Eventos

| Evento | Quando | Quem chama |
| --- | --- | --- |
| `SessionStart` | sessão abre (1ª mensagem do turno) | runtime (`agent::turn_body`) |
| `SessionEnd` | execução encerra | host CLI (`headless`, `chat`) |
| `UserPromptSubmit` | antes de gravar o prompt; bloqueio descarta, contexto entra no turno | runtime (`agent::turn_body`) |
| `PreToolUse` | antes da chamada; pode negar, perguntar, reescrever `tool_input`, injetar contexto | `Guards::before` |
| `PermissionRequest` | quando a chamada ia perguntar ao usuário; um hook pode **negar** (aprovar por hook não existe: o middleware só endurece) | `Guards::before` |
| `PostToolUse` | depois de uma chamada bem-sucedida; o contexto vai ao modelo | `Guards::after` |
| `PostToolUseFailure` | depois de uma chamada que falhou ou foi recusada | `Guards::after` |
| `Stop` | o modelo termina o turno; `continue:false`/deny pede ao runtime que continue | runtime (`agent::turn_body`) |
| `PreCompact` | antes de compactar o contexto (bloqueio pula a sumarização) | runtime (`agent::shrink`) |

## Configuração

```toml
[[hooks.PreToolUse]]
matcher = "shell|write_*"   # nomes de ferramenta separados por |, glob com *
command = "python .zerith/guard.py"
timeout_ms = 10000          # padrão 60000
```

`matcher` vale para os eventos de ferramenta (`PreToolUse`, `PostToolUse`,
`PostToolUseFailure`, `PermissionRequest`); vazio ou ausente casa tudo. Cada
entrada é `ferramenta` (glob) ou `ferramenta(especificador)`, como nas regras
de permissão: `shell(git push *)`, `write_file(*.env)|read_file`. O
especificador casa o comando (`shell`, analisado como em `permissions.md`), o
caminho ou a URL; sem esse dado a entrada não casa. Hooks de todas as camadas somam e rodam na ordem
usuário → projeto → local.

## Execução

- Windows: `cmd /C <command>`; demais: `sh -c <command>`.
- Diretório: `cwd` do payload, se existir.
- stdin: um objeto JSON
  `{event, session_id, tool_name?, tool_input?, tool_output?, prompt?, cwd}`.
- Timeout real: o processo é morto (`taskkill /T /F` no Windows; no Unix só o
  `sh` é morto — trabalho em background do hook sobrevive).

## Resultado

| Saída | Efeito |
| --- | --- |
| exit 0, stdout vazio | nada |
| exit 0, stdout JSON | `{decision?: "allow"\|"deny"\|"ask", reason?, updated_input?, additional_context?, continue?: bool}` |
| exit 0, stdout não-JSON | texto vira `additional_context` |
| exit 2 | bloqueia; stderr é o motivo |
| outro código, timeout, falha ao iniciar | erro não bloqueante em `errors` (**fail-open**: a chamada segue; `Guards::take_errors` entrega para log) |

Em `PreToolUse`, `updated_input` substitui os argumentos e a **política roda de
novo** sobre eles: um hook não consegue permitir o que a política nega.

Vários hooks: vence a decisão mais forte (deny > ask > allow); o primeiro
bloqueio interrompe os seguintes; o último `updated_input` vale.

## Segurança

Hooks rodam com os privilégios do usuário. Um `settings.toml` de projeto
pode declarar hooks: abra projetos de terceiros sabendo disso.
**Planejado:** confirmação antes de rodar hooks de camada de projeto.
