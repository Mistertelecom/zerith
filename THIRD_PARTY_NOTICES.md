# Avisos de terceiros

O Zerith é distribuído sob **GPL-3.0-only** (texto em [LICENSE](LICENSE)).

## Código incorporado

| Componente | Licença | Onde | Aviso |
| --- | --- | --- | --- |
| Rune — crates vendorizados (`accesskit_macos`, `accesskit_windows`, `parley_engine`, `wgpu-hal`, `winit`) | ver o `Cargo.toml` / `LICENSE*` de cada diretório em `vendor/` (MIT/Apache-2.0 nos originais) | `vendor/` | mantido em cada diretório |

## Dependências

Crates Rust, com versão e licença: [DEPENDENCY_LICENSES.md](DEPENDENCY_LICENSES.md)
(gerado; regenere com `python scripts/dependency-licenses.py`).

Licenças com obrigação além de "manter o aviso":

- **MPL-2.0** (`symphonia*`, `spellbook`, `opuscule`, via Rune): arquivos
  modificados dessas crates precisam ficar disponíveis sob MPL. Não
  modificamos nenhum.
- **CDLA-Permissive-2.0** (`webpki-roots`): manter o texto da licença ao
  redistribuir os dados.
- **Unicode-3.0**, **BSL-1.0**, **ISC**, **Zlib**, **BSD-***: manter aviso.

## Formatos e APIs usados sem código

Agent Skills (Apache-2.0, formato), Hermes Agent (MIT, schema), 9Router
(MIT, API HTTP). OpenHuman (GPL-3.0-only): referência de design, tokens
reproduzidos sem copiar arquivos. Claude Code e DeepSeek Harness: só
conceitos, ver
[UPSTREAMS.md](UPSTREAMS.md).

## Pendente

Um instalador precisa juntar os textos completos das licenças
(`LICENSE-*`) no pacote. Ainda não existe instalador.
