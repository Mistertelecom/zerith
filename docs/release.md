# Release, instalação e desinstalação

Nada aqui foi publicado: este documento descreve o caminho pretendido e o que
já existe. Onde não há evidência, está dito.

## O que sai no pacote

| Plataforma | Binários |
| --- | --- |
| Windows | `zerith.exe`, `zerith-ui.exe` |
| Linux | `zerith`, `zerith-ui` |
| macOS | `zerith`, `zerith-ui` |

O workflow `.github/workflows/zerith-release.yml` roda `cargo test` e
`cargo build --release -p zerith-cli -p zerith-ui` na matriz
`windows-latest`, `ubuntu-latest`, `macos-latest`, empacota com
`SHA256SUMS.txt` e anexa as limitações conhecidas às notas.

**Não executado ainda.** Só o Windows foi compilado nesta máquina. A matriz do
workflow é a intenção declarada, não um resultado.

## Instalação portátil (única forma suportada hoje)

Não há instalador nem desinstalador. Copiar os dois binários e rodar:

```bash
./zerith doctor        # cria o banco, roda integrity_check
./zerith-ui            # janela
```

Os dados ficam fora da pasta dos binários, então apagar a pasta desinstala sem
perder conversas. Para apagar tudo, apague também a raiz de dados:

| SO | Raiz |
| --- | --- |
| Windows | `%APPDATA%\Zerith` |
| macOS | `~/Library/Application Support/Zerith` |
| Linux | `$XDG_DATA_HOME/zerith` ou `~/.local/share/zerith` |

`ZERITH_HOME` sobrescreve a raiz: é assim que se roda portátil num pendrive.

## Atualização

Substituir os binários. O banco é migrado na abertura: as migrações são
versionadas (`schema_migrations`) e aplicadas em ordem, dentro de transação.
Uma versão mais nova do schema **não** abre em um binário mais antigo — o
`doctor` diz a versão em vez de falhar em silêncio.

Antes de atualizar, `zerith backup` tira uma cópia consistente pela API do
SQLite (nunca cópia de arquivo WAL vivo). `zerith restore <arquivo>` exige
`integrity_check` no backup e salva o banco atual como
`before-restore-<stamp>.db` antes de trocar.

## Assinatura

Nenhum artefato é assinado. Não há certificado de código, nem notarização de
macOS, nem repositório apt. `SHA256SUMS.txt` permite conferir o download, não
prova autoria.

## Benchmarks

`zerith bench` roda em banco descartável dentro de `ZERITH_HOME` e imprime:

| Medida | Windows, dev build, -j 1 |
| --- | --- |
| `db_open_and_migrate` | 15.38 ms |
| `append_1000_messages` | 138.44 ms (0.14 ms cada) |
| `page_50_messages` | 0.20 ms |
| `recall_200_facts` | 0.59 ms (12 hits) |
| `db_size` | 212992 bytes |
| `rss_after_load` | 8048 KB |

Build de desenvolvimento, não release; os números servem de linha de base, não
de promessa. Tempo de CPU não é medido: o host não tem chamada portátil de
CPU-time no `std` e inventar um número seria pior que documentar o proxy (tempo
de parede).

## Idle

A janela não usa polling. Ela acorda por evento do turno (`AppHandle::update`)
e por clique; um turno em voo roda em thread própria e termina sozinho.

## O que falta para uma release de verdade

1. Executar a matriz em CI e publicar os artefatos.
2. Assinatura de código (Windows Authenticode, notarização macOS).
3. Instaladores com entrada de menu e desinstalador.
4. Rodar a UI em Linux e macOS de verdade (só o build é verificado hoje).
