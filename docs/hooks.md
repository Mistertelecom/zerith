# Hooks

Comandos do usuário executados em pontos fixos da sessão. Contrato
inspirado no do Claude Code (só o conceito; nenhum código copiado): os nomes
de evento e os campos principais de entrada e saída são os dele, então muitos
scripts escritos para o Claude Code rodam sem mudança.

**Estado:** todos os 14 eventos estão ligados.
`PreToolUse`, `PermissionRequest`, `PostToolUse` e `PostToolUseFailure` rodam
por chamada de ferramenta via `zerith_runtime::guards::Guards` (middleware em
`AgentOptions.middleware`). No turno (`zerith-runtime/src/agent.rs`), quando
`TurnContext.settings` está presente, o runtime dispara `SessionStart` (na 1ª
mensagem da sessão), `UserPromptSubmit` (antes de gravar o prompt),
`PreCompact`/`PostCompact` (em volta da compactação), `Stop` (quando o modelo
termina sem chamadas de ferramenta) e `Notification`. Os subagentes disparam
`SubagentStart`/`SubagentStop` (`subagent.rs`) e a lista de tarefas,
`TaskCompleted` (`tasks.rs`). `SessionEnd` é disparado ao encerrar pelos hosts
CLI (`headless` e `chat`). Os hosts (`zerith -p`, `zerith chat` e
`zerith-desktop`) também aplicam os hooks de plugins habilitados e revisados no
`Settings` do turno.

## Eventos

| Evento | Quando | Pode bloquear? | `matcher` casa |
| --- | --- | --- | --- |
| `SessionStart` | sessão abre (1ª mensagem do turno) | não | — |
| `SessionEnd` | execução encerra (host CLI) | não | — |
| `UserPromptSubmit` | antes de gravar o prompt; contexto entra no turno | sim: descarta o prompt | — |
| `PreToolUse` | antes da chamada; pode negar, perguntar, trocar `tool_input`, injetar contexto | sim | ferramenta |
| `PermissionRequest` | a chamada ia perguntar ao usuário; um hook pode **negar** (aprovar por hook não existe: o middleware só endurece) | sim | ferramenta |
| `PostToolUse` | depois de uma chamada bem-sucedida; o contexto vai ao modelo | não (vira aviso ao modelo) | ferramenta |
| `PostToolUseFailure` | depois de uma chamada que falhou ou foi recusada | não | ferramenta |
| `Stop` | o modelo termina o turno | sim: o turno continua com o motivo como próxima mensagem | — |
| `SubagentStart` | um subagente vai começar; o contexto vai para o subagente | não | tipo do agente (`explore`, `general`, ...) |
| `SubagentStop` | um subagente terminou | sim, se terminou com `done`: volta a trabalhar com o motivo (até 3 vezes) | tipo do agente |
| `PreCompact` | antes de sumarizar o contexto | sim: pula a sumarização | gatilho (`auto`) |
| `PostCompact` | depois de compactar (passes baratos, resumo e/ou descarte) | não | gatilho (`auto`) |
| `Notification` | o Zerith precisa da pessoa (ver abaixo); roda em segundo plano | não | tipo da notificação |
| `TaskCompleted` | `task_update` vai marcar uma tarefa `completed` | sim: a tarefa fica aberta e o modelo lê o motivo | — |

Tipos de `Notification` (`notification_type`):

| Tipo | Quando |
| --- | --- |
| `permission_prompt` | um pedido de aprovação está esperando (inclusive de subagente) |
| `ask_user` | o modelo fez uma pergunta com `ask_user` |
| `idle_prompt` | um turno de mais de 60 s terminou e o Zerith espera a próxima mensagem |
| `monitor_wake` | um monitor teve linhas novas e acordou a conversa dentro do turno |

`Notification` é só informativo: o turno não espera o hook (ele roda numa
thread própria) e a saída é ignorada. Serve para ligar uma notificação do SO
ou do telefone.

## Configuração

```toml
[[hooks.PreToolUse]]
matcher = "shell|write_*"   # nomes de ferramenta separados por |, glob com *
command = "python .zerith/guard.py"
timeout_ms = 10000          # padrão 60000
```

Nos eventos de ferramenta (`PreToolUse`, `PostToolUse`, `PostToolUseFailure`,
`PermissionRequest`) o `matcher` casa a ferramenta; vazio ou ausente casa tudo.
Cada entrada é `ferramenta` (glob) ou `ferramenta(especificador)`, como nas
regras de permissão: `shell(git push *)`, `write_file(*.env)|read_file`. O
especificador casa o comando (`shell`, analisado como em `permissions.md`), o
caminho ou a URL; sem esse dado a entrada não casa. Em `Notification`,
`SubagentStart`/`SubagentStop` e `PreCompact`/`PostCompact` o `matcher` casa o
tipo da tabela acima (`permission_prompt|ask_user`, `explore`). Nos demais
eventos ele é ignorado. Hooks de todas as camadas somam e rodam na ordem
usuário → projeto → local.

## Execução

- Windows: `cmd /C <command>`; demais: `sh -c <command>`.
- Diretório: `cwd` do payload, se existir.
- stdin: um objeto JSON
  `{event, hook_event_name, session_id, tool_name?, tool_input?, tool_output?,
  prompt?, cwd, ...}` mais os campos do evento:

| Evento | Campos extras |
| --- | --- |
| `SubagentStart` | `agent_id` (`sub-N`), `agent_type`, `agent_session_id`, `description`, `model` |
| `SubagentStop` | os de cima + `stop_reason` (`done`, `max_steps`, `stopped`, `cancelled`, `timed_out`, `failed`, `panic`), `steps`, `tokens {prompt, completion, cache_read, cache_write}`, `last_assistant_message` (só em `done`), `stop_hook_active` (já foi devolvido por um hook) |
| `PreCompact` | `trigger` |
| `PostCompact` | `trigger`, `tokens_before`, `tokens_after`, `deduped`, `demoted`, `dropped`, `summarized`, `summary {messages, start_id, end_id, summary_id}` |
| `Notification` | `notification_type`, `message`, `title` |
| `TaskCompleted` | `task_id`, `task_subject`, `task_description` |

Em `SubagentStart`/`SubagentStop`, `session_id` é a conversa que delegou.

- Timeout real: o processo é morto (`taskkill /T /F` no Windows; no Unix só o
  `sh` é morto — trabalho em background do hook sobrevive).

## Resultado

| Saída | Efeito |
| --- | --- |
| exit 0, stdout vazio | nada |
| exit 0, stdout JSON | `{decision?: "allow"\|"deny"\|"ask"\|"block", reason?, updated_input?, additional_context?, continue?: bool}` |
| exit 0, stdout JSON (formato Claude Code) | `{hookSpecificOutput: {permissionDecision?, permissionDecisionReason?, updatedInput?, additionalContext?}}` |
| exit 0, stdout não-JSON | texto vira `additional_context` |
| exit 2 | bloqueia; stderr é o motivo |
| outro código, timeout, falha ao iniciar | erro não bloqueante em `errors` (**fail-open**: a chamada segue; `Guards::take_errors` entrega para log, o turno grava `hook/error`) |

`block` vale como `deny` e `approve` como `allow` (nomes do Claude Code). Uma
palavra desconhecida em `decision` é ignorada. Os dois formatos podem vir
juntos; o de `hookSpecificOutput` tem precedência.

Vários hooks: vence a decisão mais forte (deny > ask > allow); o primeiro
bloqueio interrompe os seguintes; o último `updated_input` vale.

### `PreToolUse` trocando a entrada

`updated_input` (ou `hookSpecificOutput.updatedInput`) é o objeto de
argumentos **inteiro** que substitui o do modelo (não é mescla: repita os
campos que ficam). Um valor que não é objeto é ignorado e vira erro de hook.

Sobre a entrada nova roda **tudo de novo**: as regras de ferramenta externa
(`mcp__*`, `skill(...)`), a política (`allow`/`ask`/`deny`, sandbox, limites
do workspace) e o pedido de aprovação. Um hook nunca pula uma aprovação: se
ele troca `git status` por `rm -rf build`, a aprovação é pedida para
`rm -rf build`. O modelo recebe um aviso dizendo que os argumentos foram
alterados e para quê, e o log da sessão ganha `tool/rewritten` com
`{id, name, from, to}`.

## Exemplos

**Junos: todo `commit` vira `commit confirmed 5`** (rollback automático em 5
minutos se ninguém confirmar). O hook recebe a chamada de `terminal_send` e
devolve a entrada trocada:

```toml
[[hooks.PreToolUse]]
matcher = "terminal_send|terminal_batch"
command = "python ~/.zerith/hooks/junos_commit.py"
```

```python
# junos_commit.py
import json, re, sys
call = json.load(sys.stdin)
args = call.get("tool_input", {})
fix = lambda c: re.sub(r"^\s*commit\s*(and-quit)?\s*$", "commit confirmed 5", c)
if "input" in args:
    new = fix(args["input"])
    changed = new != args["input"]
    args["input"] = new
else:
    cmds = args.get("commands", [])
    new = [fix(c) for c in cmds]
    changed = new != cmds
    args["commands"] = new
if changed:
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "updatedInput": args,
        "additionalContext": "commit trocado por commit confirmed 5; confirme com `commit` depois de validar",
    }}))
```

O comando novo continua sendo uma mudança em equipamento de rede, então a
aprovação é pedida normalmente.

**`Notification` no Linux (`notify-send`):**

```toml
[[hooks.Notification]]
matcher = "permission_prompt|ask_user"
command = 'notify-send Zerith "$(jq -r .message)"'
```

**`Notification` no Windows (toast do PowerShell):**

```toml
[[hooks.Notification]]
command = "powershell -NoProfile -Command \"$m=([Console]::In.ReadToEnd()|ConvertFrom-Json).message; New-BurntToastNotification -Text 'Zerith',$m\""
```

(`New-BurntToastNotification` vem do módulo BurntToast:
`Install-Module BurntToast`.) Para o telefone, troque por um `curl` para o
ntfy, Pushover ou bot do Telegram.

**Subagente só termina com testes verdes:**

```toml
[[hooks.SubagentStop]]
matcher = "general"
command = "cargo test -q >/dev/null 2>&1 || (echo 'os testes falham; corrija antes de terminar' >&2; exit 2)"
```

**Registro das compactações:**

```toml
[[hooks.PostCompact]]
command = "cat >> ~/.zerith/compactions.jsonl"
```

## Segurança

Hooks rodam com os privilégios do usuário. Um `settings.toml` de projeto
pode declarar hooks: abra projetos de terceiros sabendo disso.
**Planejado:** confirmação antes de rodar hooks de camada de projeto.

## Limites

- `Notification` não cobre o pedido de aprovação mostrado pelo próprio host
  fora de um turno, nem o monitor que acorda uma conversa já parada no
  desktop (`chat::spawn_monitor_waker`): o turno que ele inicia dispara os
  eventos normais.
- `PostCompact` do caminho antes do turno (`auto_compact_routed`) roda no
  diretório do processo, não no da sessão.
- `/compact` manual não dispara `PreCompact`/`PostCompact`.
