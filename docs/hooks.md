# Hooks

Comandos do usuário executados em pontos fixos da sessão. Contrato
inspirado no do Claude Code (só o conceito; nenhum código copiado).

**Estado:** biblioteca pronta e testada (`zerith_core::hooks::run_hooks`).
**Planejado:** o runtime ainda não dispara os eventos.

## Eventos

`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`,
`PreCompact`, `SessionEnd`.

## Configuração

```toml
[[hooks.PreToolUse]]
matcher = "shell|write_*"   # nomes de ferramenta separados por |, glob com *
command = "python .zerith/guard.py"
timeout_ms = 10000          # padrão 60000
```

`matcher` só vale para `PreToolUse`/`PostToolUse`; vazio ou ausente casa
tudo. Hooks de todas as camadas somam e rodam na ordem
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
| outro código, timeout, falha ao iniciar | erro não bloqueante em `errors` |

Vários hooks: vence a decisão mais forte (deny > ask > allow); o primeiro
bloqueio interrompe os seguintes; o último `updated_input` vale.

## Segurança

Hooks rodam com os privilégios do usuário. Um `settings.toml` de projeto
pode declarar hooks: abra projetos de terceiros sabendo disso.
**Planejado:** confirmação antes de rodar hooks de camada de projeto.
