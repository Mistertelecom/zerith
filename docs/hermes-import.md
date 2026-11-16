# Importar do Hermes Agent

Read-only: o banco de origem é aberto só para leitura e nunca é alterado.

```bash
zerith import detect                    # procura ~/.hermes/state.db (e perfis)
zerith import hermes [--profile <nome>]
zerith import hermes --jsonl export.jsonl [--profile <nome>]   # `hermes sessions export`
```

- Schema verificado contra hermes-agent rev `4162ecf1`.
- Colunas desconhecidas são preservadas em `raw`.
- Repetir não duplica: chave única por (`source_id`, perfil).
- **Retomada incremental por mensagem:** no SQLite, `cursor()` lê
  `MAX(json_extract(raw, '$.id'))` das mensagens já importadas e copia apenas
  `id > after` (ativas); no JSONL, `import_jsonl` conta as mensagens já
  gravadas na sessão (`store.message_count`) e salta esse prefixo (`skip(have)`).
  Sessões já existentes que recebem novas mensagens contam em `sessions_resumed`.
- **Pré-visualização e seleção (`zerith-importers`):** `preview(path)` lê
  somente leitura e devolve `SourcePreview` (sessões, contagem de mensagens
  ativas, flag `archived`, `total_messages`, `min_date` e `max_date`).
  `import_selected(path, profile, store, include_archived, only_ids)` permite
  filtrar sessões arquivadas e restringir por IDs selecionados.

## Interface na janela (`zerith-ui`)

Na tela **Histórico**, o botão **Importar Hermes** abre um painel visual com:

- detecção automática de `state.db` (com escolha de perfil quando há mais de um)
  e campo para caminho manual (`.db` ou `.jsonl`) com botão **Inspecionar**;
- interruptor **Incluir arquivadas**;
- pré-visualização com contagens de sessões/mensagens e datas mínima e máxima;
- seleção individual de sessões e botão **Selecionar todas / limpar**;
- botão **Importar selecionadas** e relatório final (`importadas`, `retomadas`,
  `ignoradas`, `mensagens` e eventuais falhas).

`--jsonl` lê um arquivo de exportação: uma linha por sessão (com `messages`)
ou por mensagem, mesma chave de idempotência e `raw` para o que não é mapeado.
Sem `--profile`, o perfil gravado é `jsonl`. Cobertura: os testes de
`zerith-importers/tests/hermes.rs`, `zerith-importers/tests/hermes_jsonl.rs` e
`zerith-ui/tests/shell.rs`; o JSONL ainda **não** foi testado contra uma
exportação real do Hermes nesta máquina.

Relatório: sessões vistas/importadas/puladas/retomadas, mensagens
importadas/puladas, falhas (uma por linha em stderr no CLI, ou exibidas no
painel da UI).
