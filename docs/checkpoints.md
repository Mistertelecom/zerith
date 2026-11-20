# Checkpoints e rewind

Desfaz o que um turno fez nos **arquivos** e na **conversa**, como o `/rewind`
do Claude Code. Código em `zerith-sessions/src/checkpoints.rs`.

## Como funciona

1. Antes de cada turno que pode editar arquivos, o host abre um checkpoint:
   `Checkpoint::begin(store, data_root, session, turn)`. O ponto de corte da
   conversa é a última mensagem viva **antes** do prompt do turno.
2. No **primeiro passo que muda algo** no turno (a primeira edição de arquivo
   ou o primeiro comando de `shell` que não é só leitura, em primeiro ou
   segundo plano), o checkpoint tira um **snapshot do workspace inteiro**
   (`Checkpoint::snapshot`, em `checkpoints/shadow.rs`). É um repositório git
   à parte, em `<raiz de dados>/checkpoints/.shadow/<hash do workspace>/`,
   usado com `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` próprios: o `.git` e o
   índice do usuário nunca são tocados, e funciona em pasta sem git. O índice
   dele fica salvo, então do segundo snapshot em diante o git só relê o que
   mudou. O id da árvore vai para `<pasta do turno>/shadow.tree`. Comando só
   leitura (`ls`, `cat`, `git status`…, segundo `shellparse::all_read_only`)
   não dispara nada.
3. Antes de `write_file` ou `edit_file` alterar algo, ela chama
   `exec.host.before_write(&path)` (ligado a `checkpoint.record_file(path)`
   pelo runtime). A primeira chamada por arquivo e por turno
   copia o conteúdo anterior para
   `<raiz de dados>/checkpoints/<sessão>/<turno>/`; um arquivo que o turno
   **cria** é registrado como "não existia". Chamar de novo é inofensivo.
4. `restore(store, data_root, session, turn, mode)` volta ao estado de antes do
   turno: `Code` (arquivos), `Conversation` (mensagens) ou `Both`. Com
   snapshot, `Code` deixa os arquivos rastreados do workspace como estavam:
   reescreve o que mudou ou sumiu e **apaga o que o turno criou** (e as pastas
   que ficaram vazias), seja qual for a ferramenta que mexeu. Turno a turno,
   do mais novo ao mais antigo, as cópias por arquivo são aplicadas antes do
   snapshot do mesmo turno: onde o snapshot cobre, ele vence (uma cópia pode
   ser posterior a um `rm` do shell); fora dele (ignorados, fora do workspace)
   valem as cópias. O estado substituído fica guardado no repositório sombra e
   o relatório traz o comando git para recuperá-lo.

O esquema do banco é a migração **v6** (`messages.superseded` e a tabela
`checkpoints`). Os arquivos copiados ficam fora do banco, então quem registra
nunca precisa de uma conexão SQLite.

## Limites, ditos sem enfeite

- **Arquivos ignorados ficam de fora do snapshot**: os `.gitignore` do
  workspace, o excludes global do git e a lista fixa `node_modules/`,
  `target/`, `.venv/`, `venv/`, `__pycache__/`, `.gradle/`, `dist/`. O
  rewind não os traz de volta nem os apaga; só voltam se uma ferramenta de
  arquivo os editou (cópia por arquivo). Um `.env` apagado pelo shell não volta.
- **Sem snapshot, só as ferramentas de arquivo são desfeitas** (o
  comportamento antigo). Acontece quando o workspace é a pasta pessoal do
  usuário, a raiz de um drive ou a pasta de dados do Zerith; quando não há
  `git` no `PATH`; ou quando o snapshot passa de **20 s** (aí ele é
  desligado por 24 h naquele workspace, para não custar 20 s a cada turno). O
  comando roda mesmo assim, o motivo fica em `shadow.failed` e a prévia e o
  restore (`zerith rewind`, botão Rebobinar) avisam.
- Desfazer o snapshot também desfaz o que **outros processos** mudaram nos
  arquivos rastreados durante o turno (um editor aberto, outro agente).
- Restaurar **sobrescreve** o que está no disco agora, inclusive edições que
  você fez à mão depois do turno. `zerith rewind` sem modo só mostra o que
  mudaria, e não altera nada.
- Não é controle de versão. Continue usando git.
- Teto de **10 MiB por arquivo** e **100 MiB por turno**. Passou disso,
  `record_file` devolve um erro claro e a ferramenta **não deve editar**.
- Guarda os **últimos 100 turnos** por sessão; os mais antigos perdem os
  arquivos copiados e o ponto de rewind.
- Um turno já restaurado (`Code`) não pode ser reaplicado: as cópias são
  consumidas, porque reaplicá-las apagaria trabalho mais novo.
- Rewind da conversa é **suave**: as mensagens ganham `superseded = 1` e somem
  de `messages()`, `recent_messages()`, `message_count()`, busca e fork, mas
  ficam no arquivo. Só `--purge` (ou `purge_superseded`) as apaga de vez, e isso
  não tem volta.

## CLI

```bash
zerith checkpoints <id|nome>                       # turnos, arquivos, mensagens após o ponto
zerith rewind <id|nome>                            # prévia do último turno; nada muda
zerith rewind <id|nome> --turn 3 --code            # só arquivos
zerith rewind <id|nome> --turn 3 --conversation    # só mensagens (arquivadas)
zerith rewind <id|nome> --turn 3 --both
zerith rewind <id|nome> --turn 3 --both --purge    # e apaga as arquivadas de vez
```

`zerith-runtime/src/agent.rs` (`run_agent_turn`) abre um checkpoint por turno
quando o turno tem ferramentas (`Registry`) e banco em arquivo (cobrindo `-p`,
`chat` e `zerith-desktop`). Na janela (`zerith-desktop`), o botão **Rebobinar** (com
resumo de arquivos e linhas alteradas do turno) e o comando `/rewind`
restauram o último checkpoint (`Mode::Both`).

## Ligação no runtime e nas ferramentas

- **Runtime (`zerith-runtime/src/agent.rs`):** antes de gravar a mensagem do
  usuário do turno (para o ponto de corte incluir o prompt), `turn_body` calcula
  `checkpoints::next_turn`, abre `Checkpoint::begin(&store, &root, session_id, turn)`
  e registra `r.host().on_before_write(move |p| cp.record_file(p).map_err(|e| e.to_string()))`.
  Se abrir o checkpoint falhar, o aviso vai para `stderr` e o turno segue sem
  rewind (nunca aborta o turno).
- **Ferramentas (`zerith-tools/src/builtin.rs` e `host.rs`):** `write_file` e
  `edit_file` chamam `exec.host.before_write(&path)?` após `check_editable(&path)?`
  e antes de gravar no disco (a política já foi aplicada pelo pipeline). Se
  `record_file` devolver erro (ex.: arquivo acima de 10 MiB ou turno acima de
  100 MiB), `before_write` cancela a edição e devolve o erro ao modelo.
- **Shell (`builtin.rs::shell`):** antes de rodar, inclusive com
  `run_in_background`, chama `exec.host.before_shell(cmd, workspace)`, que
  pula comandos só leitura e chama o hook `on_before_shell`; o runtime liga os
  dois hooks a `checkpoint.snapshot(workspace)`. Erro do snapshot **nunca**
  cancela o comando (ao contrário de `record_file`, que cancela a edição).
