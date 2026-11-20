# Release, instalação e desinstalação

Nada aqui foi publicado: este documento descreve o caminho pretendido e o que
já existe. Onde não há evidência, está dito.

## O que sai no pacote

| Plataforma | Arquivo | Conteúdo |
| --- | --- | --- |
| Windows (CLI) | `zerith-<tag>-windows-x86_64.zip` + `.sha256` | `zerith.exe` |
| Windows (Desktop) | `Zerith_<ver>_x64-setup.exe` + `.sha256` + `.sig` | Instalador NSIS do `zerith-desktop` (Tauri 2) assinado com Ed25519 |
| Linux (CLI) | `zerith-<tag>-linux-x86_64.tar.gz` + `.sha256` | `zerith` |
| macOS (CLI) | `zerith-<tag>-macos-<arch>.tar.gz` + `.sha256` | `zerith` |

Cada arquivo portátil traz também README, LICENSE, avisos de terceiros e estes
docs. O instalador desktop NSIS no Windows é usado pelo auto-updater embutido
(`crates/zerith-desktop/src/update.rs`), que verifica obrigatoriamente tanto o
`.sha256` quanto a assinatura Ed25519 (`.sig`) contra a chave pública embutida
no binário.

O workflow `.github/workflows/zerith-release.yml` roda, na matriz
`windows-latest`, `ubuntu-latest`, `macos-latest`: `cargo test --workspace`,
`cargo build --release -p zerith-cli`, empacota os arquivos portáteis, e no
Windows constrói e assina o instalador NSIS do `zerith-desktop`. Numa tag
`zerith-v*`, um job final cria um **rascunho** de release no GitHub com os
arquivos e as limitações conhecidas; uma execução manual só gera artefatos.

## Instalação

- **Windows (desktop):** execute `Zerith_<ver>_x64-setup.exe` (instalador NSIS
  com atalho, desinstalador e atualização automática assinada).
- **Portátil (CLI):** copie `zerith` (ou `zerith.exe`) e rode:

```bash
./zerith doctor        # cria o banco, roda integrity_check
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

## Assinatura do auto-updater (Ed25519)

O instalador Windows (`Zerith_<ver>_x64-setup.exe`) carrega uma assinatura
Ed25519 destacada (`.sig`) sobre a mensagem canônica:

```text
zerith-update-v1
version=<versão>
file=<nome-do-instalador>
sha256=<digest-sha256-hex>
```

Como configurar as chaves uma única vez:

1. Gere o par de chaves localmente:
   ```bash
   node scripts/sign-update.mjs keygen
   ```
2. Cole o valor de `ZERITH_UPDATE_PUBKEY` (32 bytes em base64) em
   `crates/zerith-desktop/update-pubkey.txt` e commite no repositório.
3. Cadastre o valor de `ZERITH_UPDATE_SIGNING_KEY` nos Secrets do GitHub
   (**Settings -> Secrets and variables -> Actions**). **Nunca commite a chave
   privada.**

No release (`zerith-v*`), o workflow valida que `ZERITH_UPDATE_SIGNING_KEY`
corresponde a `update-pubkey.txt`, embute a chave pública no binário via
`ZERITH_UPDATE_PUBKEY` e gera + verifica `.sha256` e `.sig`. Se `.sig` ou
`.sha256` estiverem ausentes ou inválidos — ou se o build não tiver chave
pública configurada — o auto-updater falha fechado e recusa instalar.

**Assinatura de SO (Authenticode / Gatekeeper):** ainda não há certificado
Authenticode do Windows nem notarização do macOS, então o SmartScreen/Gatekeeper
pode exibir aviso na primeira instalação manual. Para atualizações feitas pelo
próprio app, a assinatura Ed25519 garante autoria e integridade contra a chave
pública embutida.

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

1. Executar a matriz em CI uma vez (o workflow existe, nunca rodou).
2. Assinatura de código (Windows Authenticode, notarização macOS).
3. Instaladores com entrada de menu e desinstalador.
4. Rodar a UI em Linux e macOS de verdade (só o build é verificado hoje).
