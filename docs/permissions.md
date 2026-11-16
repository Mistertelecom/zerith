# Permissões

Modelo inspirado no do Claude Code (só o conceito; nenhum código copiado).
Uma política única sai das configurações e do ambiente:

```rust
let policy = Policy::from_settings(&loaded.settings, workspace)?;
```

Typo em preset, modo ou regra é **erro**, nunca um padrão mais largo.

## Presets e modos

O **preset** é o teto (o que o sandbox permite): `read-only`,
`workspace-write` (padrão), `danger-full-access` (`permissions.mode` ou
`ZERITH_SANDBOX`). O **modo** diz com que frequência o usuário é consultado
abaixo do teto (`permissions.default_mode` ou `ZERITH_MODE`; qualquer grafia:
`acceptEdits` ou `accept-edits`):

| Modo | Efeito |
| --- | --- |
| `default` | leitura no workspace roda; escrita, comando e rede perguntam |
| `acceptEdits` | escrita dentro do workspace roda; o resto pergunta |
| `plan` | só leitura; escrita e shell são **negados** (ver "Plano") |
| `dontAsk` | tudo que perguntaria é **negado**; só roda o que uma regra `allow`, o preset ou o modo já aprovam |
| `bypassPermissions` | tudo roda, **exceto** o que uma regra `deny`/`ask` ou o preset barra |

Sem `default_mode`, vale o implícito do preset (`workspace-write` →
`acceptEdits`, `danger-full-access` → bypass, `read-only` → `default`).
`permissions.mode = "plan"` (nome de modo no lugar de preset) também vale.

`ModeHandle` (`Arc<Mutex<Mode>>`) troca o modo em tempo de execução:
`policy.mode_handle()` devolve o handle compartilhado por todas as cópias da
política; a UI chama `handle.set(Mode::Default)`.

## Regras

```toml
[permissions]
allow = ["shell(cargo test *)", "read_file", "mcp__docs__*", "skill(deploy)"]
ask   = ["shell(git push *)"]
deny  = ["read_file(./.env)", "write_file(src/secret/**)", "shell(rm *)"]
```

Ordem: **deny → ask → allow**; a primeira regra que casa vale. Um `allow` nunca
desfaz um `deny` (nem de outra camada); as listas somam entre camadas. Env
`ZERITH_DENY`/`ZERITH_ASK`/`ZERITH_ALLOW` (`;`) só **acrescentam** regras.
Um `deny` vale em todos os modos, inclusive `bypassPermissions`.

Sintaxe `Ferramenta(especificador)`; sem especificador casa toda chamada;
`*` no nome da ferramenta é curinga (`mcp__github__*`).

- **Shell**: o comando é dividido em `&&`, `||`, `;`, `|`, `&`, quebra de linha,
  `$(...)`, crases e subshells; `bash -c "..."`, `cmd /c`, `powershell -Command`
  são abertos. Wrappers (`timeout`, `nice`, `nohup`, `time`, `xargs`,
  `env VAR=x`, `VAR=x cmd`) são removidos antes de casar. `deny`/`ask` casam se
  **qualquer** parte casar; `allow` só vale se **todas** as partes forem
  permitidas (`shell(git *)` não libera `git status && rm -rf x`).
  `shell(cargo test *)` casa também `cargo test` puro.
- **Caminhos**: `..` resolvido, links simbólicos resolvidos no prefixo
  existente, sem diferença de maiúsculas nem de `/` vs `\`. O especificador casa
  o caminho relativo ao workspace (`src/**`, `./.env`), o absoluto, `~/...`, ou o
  nome do arquivo quando não há `/` (`*.env`).
- **Sem ação** (MCP, skills, plugins): `Policy::check_rules(tool, subject)`. Uma
  ferramenta `mcp__*` sem regra pergunta (negada em `dontAsk`, livre em bypass).

**Casamento de texto não é fronteira de segurança.** Alias, funções, `eval`,
expansão de variáveis e scripts que o comando executa escapam da análise. A
fronteira real é o sandbox (`docs/sandbox.md`, parcial) e a aprovação do
usuário.

### Leitura sempre liberada

Comandos somente-leitura (`ls`, `cat`, `head`, `tail`, `pwd`, `echo`, `wc`,
`grep`, `rg`, `diff`, `git status|diff|log|show|rev-parse|ls-files|blame`, ...)
rodam sem perguntar em qualquer modo, inclusive `plan`, e `allow` os trata como
permitidos dentro de comandos compostos. Qualquer redirecionamento (`>`), `$()`
com escrita, `git -c`, `--output`, `rg --pre` desqualifica.

## Plano (`ExitPlanMode`)

Em `plan` o modelo recebe `exit_plan_mode{plan}`. O middleware (`guards`)
responde `Ask` com o texto do plano; aprovado, o `ModeHandle` volta ao modo
anterior (ou `acceptEdits` se plan era o início); negado, continua em `plan`.
`enter_plan_mode` faz o caminho inverso. Fora de `plan`, `exit_plan_mode` é
negado.

## Estado no código

| Parte | Estado |
| --- | --- |
| `Policy::from_settings`, regras, modos, `ModeHandle`, análise de shell | biblioteca pronta, testada |
| `zerith-runtime::guards::Guards` (regras p/ ferramentas sem ação, plano, hooks) | middleware pronto; **o chamador precisa registrá-lo** em `AgentOptions.middleware` e usar `Policy::from_settings` (hoje `zerith-cli` e a UI ainda usam `from_env`) |
| `Registry::authorize` → `Policy::check_tool` | já aplica regras, modos e shell só-leitura |
| Aprovação interativa | `ApprovalGate` do runtime |
| Sandbox de SO | parcial, ver `docs/sandbox.md` |
