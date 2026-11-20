# Avisos de terceiros

O Zerith é distribuído sob **GPL-3.0-only** (texto em [LICENSE](LICENSE)).

## Dependências

Crates Rust (incluindo Tauri 2 em `zerith-desktop`), com versão e licença:
[DEPENDENCY_LICENSES.md](DEPENDENCY_LICENSES.md) (gerado; regenere com
`python scripts/dependency-licenses.py`).

Licenças com obrigação além de "manter o aviso":

- **CDLA-Permissive-2.0** (`webpki-roots`): manter o texto da licença ao
  redistribuir os dados.
- **Unicode-3.0**, **BSL-1.0**, **ISC**, **Zlib**, **BSD-***: manter aviso.

## Formatos e APIs usados sem código

Agent Skills (Apache-2.0, formato), Hermes Agent (MIT, schema), 9Router
(MIT, API HTTP). OpenHuman (GPL-3.0-only): referência de design, tokens
reproduzidos sem copiar arquivos. Claude Code e DeepSeek Harness: só
conceitos, ver
[UPSTREAMS.md](UPSTREAMS.md).

## billion-context (programa externo, não embutido)

Conversas longas usam o [billion-context](https://github.com/ranxianglei/billion-context)
(MIT + termo de atribuição), instalado à parte com `npm install -g billion-context` e executado
como processo local. O Zerith não copia nem distribui o código dele; o crédito aparece em
Ajustes › Geral › Conversas longas, como a licença pede.

## Pendente

Um instalador precisa juntar os textos completos das licenças
(`LICENSE-*`) no pacote. Ainda não existe instalador.

## Plugins da loja `zerith` (baixados sob demanda, não embutidos)

`.claude-plugin/marketplace.json` só aponta para os repositórios abaixo; nada
deles vem no binário. Cada plugin é baixado do repositório original quando o
usuário instala e mantém a licença de lá.

| Plugin | Repositório | Licença |
| --- | --- | --- |
| claude-bughunter | https://github.com/elementalsouls/Claude-BugHunter | MIT (código) + CC BY 4.0 (conteúdo) |
| cli-anything | https://github.com/HKUDS/CLI-Anything | Apache-2.0 |
| stitch-skills | https://github.com/google-labs-code/stitch-skills | Apache-2.0 |
| astryx | https://github.com/meta/astryx | MIT |
| billion-context | https://github.com/ranxianglei/billion-context | MIT + termo de atribuição (skill escrita pelo Zerith em `plugins/billion-context`) |
