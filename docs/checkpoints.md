# Checkpoints e rewind

Desfaz o que um turno fez nos **arquivos** e na **conversa**, como o `/rewind`
do Claude Code. Código em `zerith-sessions/src/checkpoints.rs`.

## Como funciona

1. Antes de cada turno que pode editar arquivos, o host abre um checkpoint:
   `Checkpoint::begin(store, data_root, session, turn)`. O ponto de corte da
   conversa é a última mensagem viva **antes** do prompt do turno.
2. Antes de uma ferramenta de arquivo alterar algo, ela chama
   `checkpoint.record_file(path)`. A primeira chamada por arquivo e por turno
   copia o conteúdo anterior para
   `<raiz de dados>/checkpoints/<sessão>/<turno>/`; um arquivo que o turno
   **cria** é registrado como "não existia". Chamar de novo é inofensivo.
3. `restore(store, data_root, session, turn, mode)` volta ao estado de antes do
   turno: `Code` (arquivos), `Conversation` (mensagens) ou `Both`.

O esquema do banco é a migração **v6** (`messages.superseded` e a tabela
`checkpoints`). Os arquivos copiados ficam fora do banco, então quem registra
nunca precisa de uma conexão SQLite.

## Limites, ditos sem enfeite

- **Só o que passa por `record_file` é desfeito.** Mudança feita por um comando
  de `shell` (`rm`, `sed -i`, um build, `git checkout`) não é vista nem
  restaurada. É o mesmo limite do Claude Code.
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

`zerith chat` e `zerith -p` abrem um checkpoint por turno. Enquanto as
ferramentas de arquivo não chamarem `record_file` (veja abaixo), o checkpoint
guarda o ponto de corte da **conversa** e nenhum arquivo.

## API para o runtime e as ferramentas

```rust
let turn = checkpoints::next_turn(&store, &session)?;
let cp = Arc::new(Checkpoint::begin(&store, data_root, &session, turn)?);
// ... antes de append(user message) ...
```

**Pontos de chamada que faltam (para quem é dono de `zerith-tools` / `zerith-runtime`):**

- `write_file` e `edit_file`: chamar `cp.record_file(&path)?` **depois** da
  autorização da política e **antes** de gravar. Se devolver erro, não gravar e
  mostrar o erro ao modelo.
- `CallCtx` precisa carregar um `Option<&Checkpoint>` (ou o runtime passar um
  `Arc<Checkpoint>`); `Checkpoint` é `Send + Sync`, então chamadas paralelas
  podem registrar ao mesmo tempo.
- O runtime deve abrir o checkpoint **antes** de gravar a mensagem do usuário
  do turno (o ponto de corte depende dessa ordem).

Estado real: o CLI abre checkpoints; a ligação de `write_file`/`edit_file` ao
`record_file` **não está feita por este trabalho** (as ferramentas não são
deste escopo).
