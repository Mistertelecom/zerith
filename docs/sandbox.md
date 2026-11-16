# Sandbox

O que o Zerith confina de verdade ao rodar `shell`, por plataforma. A
política (`docs/permissions.md`) decide **se** um comando roda; o sandbox
limita **o que ele pode fazer** quando roda. `Policy::enforcement` é
`partial` em toda plataforma: nenhuma isola leitura e rede.

O relatório vem de `zerith_security::jail::report(level)`; `Policy::enforcement` já o usa. **Planejado:** `zerith policy` (zerith-cli) imprimir as listas isolado/não isolado; hoje imprime só `enforcement   partial`.

## API (para `zerith-tools`)

```rust
use zerith_security::jail::{Jail, Limits};

let jail = Jail::for_policy(&policy);          // ou Jail::new(Sandbox::WorkspaceWrite, ws)
let jail = jail.with_limits(Limits { max_processes: 128, memory_bytes: 4 << 30 });
let mut jailed = jail.spawn(command)?;         // preferido; único caminho que usa o Job Object
let out = jailed.child.wait_with_output()?;    // soltar `jailed` mata a árvore no Windows
jailed.kill_tree();                            // cancelamento / timeout

let cmd = jail.wrap(command);                  // só o que vale antes do spawn (macOS, grupo Unix)
let cmd = Sandbox::WorkspaceWrite.wrap(ws, command);   // atalho
```

Configure stdio **depois** de `wrap` (no macOS o programa vira argumento do
`sandbox-exec`). `spawn` falha fechado: se o processo não puder entrar no
Job Object ele é morto e o erro volta.

## Windows — verificado

Job Object criado por chamada, com o filho iniciado suspenso, anexado e só
então retomado (nunca roda fora do job):

| Isolado | Como foi verificado (testes em `jail.rs`) |
| --- | --- |
| Árvore morre ao soltar o job (`KILL_ON_JOB_CLOSE`) | `cmd` → `ping` neto; ao soltar o `Jailed` o neto desaparece |
| Limite de processos (padrão 128) | com limite 3, 6 filhos pedidos: no máximo 2 sobrevivem; controle sem limite: 6 |
| Limite de memória da árvore (padrão 4 GiB) | configurado; **sem teste de estouro** |
| Limites de UI (desktop, clipboard, átomos globais, parâmetros do sistema, logoff) | configurado; sem teste |

**Não isolado no Windows:** escrita em arquivos (sem token restrito /
baixa integridade: um shell escreve em qualquer lugar que o usuário
escreve), leitura, rede, variáveis de ambiente e credenciais do usuário.
Token restrito e AppContainer **não** foram implementados: não pude verificar
que `cargo`/`git` continuam funcionando sob eles.

`danger-full-access` mantém só `KILL_ON_JOB_CLOSE`.

## macOS — NÃO verificado

Escrito sem acesso a um Mac. `jail::macos_profile` gera um perfil
`sandbox-exec` (`(allow default)(deny file-write*)` + escrita só no workspace
e em temporários; `read-only` não permite diretório algum) e
`jail::sandbox_exec` embrulha o comando. O texto do perfil é testado em
qualquer plataforma; **a execução real não foi testada**. `sandbox-exec` é
marcado como obsoleto pela Apple. Leitura e rede não são confinadas.

## Linux — planejado

Hoje só um grupo de processos próprio (`kill_tree` mata o grupo). Landlock
exige a crate `landlock`, que não está no `Cargo.lock`; não foi adicionada.
Sem isolamento de escrita, leitura, rede, processos ou memória.

## Resumo honesto

| | escrita | leitura | rede | processos | memória | matar a árvore |
| --- | --- | --- | --- | --- | --- | --- |
| Windows | não | não | não | sim | sim (sem teste) | sim |
| macOS (não verificado) | sim | não | não | não | não | não |
| Linux | não | não | não | não | não | grupo |

As regras `shell(...)` são casamento de texto, não fronteira de segurança:
um comando que a análise não entende pode escapar delas. A fronteira é este
sandbox (parcial) mais a aprovação do usuário.
