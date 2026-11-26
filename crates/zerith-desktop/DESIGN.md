# Zerith Desktop — contrato visual

Referências: **OpenHuman** (`app/` no repo pai: shell em camadas, settings com navegação lateral
agrupada e cabeçalho de painel) e **DeepSeek Harness / Abyss** (`douyamv/deepseek-harness-studio`:
hairline no lugar de preenchimento cinza, mono como elemento de design nos cartões de ferramenta,
composer flutuante).

**Paleta (pedido do usuário):** dark = fundo preto puro, texto branco; light = branco, texto preto.
Acento monocromático (branco no dark, preto no light). Cor só para estado (ok/warn/danger). Logo:
`icons/logo-white.svg` / `logo-black.svg`, componente `<ZerithMark>`.

**Exceção: o chat (pedido do usuário, 2026-11).** A transcrição da conversa tem paleta própria,
no estilo opencode: `--chat-*` (acento pêssego, link azul, título violeta, código verde, citação e
marcadores), `--syn-*` (realce de sintaxe) e `--diff-*` (diff). Valem só dentro da transcrição
(markdown, balão do usuário, raciocínio, ferramentas, diff). A moldura (sidebar, topo, composer,
ajustes, painel) continua monocromática. Todo texto dessas paletas mede ≥ 4.5:1 sobre `--bg-base`,
`--code-bg` e `--layer-2`, nos dois temas.

Todo componente obedece a este arquivo. Conflito com ele = não faça.

## Regras duras

1. Cor só por token (`var(--…)` de `src/styles/tokens.css`). **Nenhum hex/rgba literal** em CSS de
   componente. Faltou token? Use o mais próximo e diga no relatório.
2. Ícones: `lucide-react` (já instalado). Nada de glifo de texto como ícone (`+`, `⌕`, `✓`, `−`).
   Tamanhos: 14 (inline em texto pequeno), 16 (botões, nav), 18 (cabeçalhos), 20–24 (estado vazio).
   `strokeWidth` 1.75 em 16+, 2 em 14.
3. Logos de provedor: `<Logo id=…>` de `components/ui.tsx` (assets do 9router). Sempre que uma linha
   fala de um provedor/conta/modelo, mostre o logo.
4. Foco: `:focus-visible { outline: 2px solid var(--ring); outline-offset: 2px }` — nunca
   `outline: none` sem substituto.
5. Movimento: cor/borda/sombra 0.15s, transform 0.1s, `var(--ease)`. Respeitar
   `[data-motion="reduced"]` e `prefers-reduced-motion` (sem animação).
6. Texto da UI em pt-BR. Nenhum nome de classe/behavior de teste muda sem motivo; comportamento
   (handlers, chamadas `api.*`) **não muda**.
7. Contraste: texto de corpo ≥ 4.5:1 sobre a superfície onde está; legenda (`--label-caption`) só
   para metadado, nunca para conteúdo.

## Tokens (resumo — valores em `tokens.css`)

| Papel | Token |
|---|---|
| Fundo da janela / moldura | `--bg-chrome` |
| Folha de conteúdo (cartão central) | `--bg-base` |
| Camadas: input, cartão, linha elevada, popover | `--layer-1` `--layer-2` `--layer-3` |
| Hover / pressionado (alpha, serve em qualquer fundo) | `--hover` `--active` |
| Hairlines | `--line` (padrão) `--line-strong` (foco, cartão aberto) `--line-faint` |
| Texto | `--label-primary` `--label-secondary` `--label-tertiary` `--label-caption` |
| Acento (monocromático) | `--accent` `--accent-text` `--accent-soft` `--ring` `--on-accent` |
| Chat (só na transcrição) | `--chat-accent` `--chat-link` `--chat-heading` `--chat-code` `--chat-code-bg` `--chat-quote` `--chat-marker` |
| Sintaxe / diff (só na transcrição) | `--syn-kw/str/num/cmt/type/fn/key/ip/net` · `--diff-add` `--diff-add-bg` `--diff-del` `--diff-del-bg` |
| Estado | `--ok` `--warn` `--danger` `--done` + `*-soft` (fundo) + `*-text` |
| Sombra | `--shadow-1` (encostado) `--shadow-2` (cartão) `--shadow-3` (popover/diálogo) `--shadow-composer` `--glow-accent` |
| Raio | `--r-xs 6` `--r-sm 8` `--r-md 12` `--r-lg 16` `--r-xl 22` `--r-full` |
| Tipo | `--fs-caption 11` `--fs-small 12` `--fs-ui 13` `--fs-body 14` `--fs-title 15` `--fs-h2 18` `--fs-hero 26` |
| Fonte | `--font` (Inter → Segoe UI Variable) `--mono` (JetBrains Mono → Cascadia) |

## Primitivos (em `styles/base.css`, componentes em `components/ui.tsx`)

- `.btn` (secundário: hairline + `layer-1`, hover `layer-2`), `.btn-primary` (acento cheio,
  `--on-accent`), `.btn-ghost` (sem borda), `.btn-danger` (só contorno/texto `--danger`, hover
  `--danger-soft`), `.btn-sm`. Altura 32 (sm 28), raio `--r-sm`, ícone 16 + gap 6.
- `.icon-btn` 32×32 (`.icon-btn.sm` 28), raio `--r-sm`, cor `--label-tertiary`, hover `--hover` +
  `--label-primary`.
- `.input`, `.select`, `textarea.input`: `layer-1`, hairline `--line`, raio `--r-sm`, altura 34;
  foco = borda `--accent` + `box-shadow: 0 0 0 3px var(--accent-soft)`.
- `.field` (rótulo 12/500 `--label-secondary` + controle + `.field-hint` 12 `--label-tertiary`).
- `.badge` (pílula 11/600, `layer-2`, hairline) + `.badge.ok|warn|danger|accent` (fundo `*-soft`,
  texto `*-text`). `.kbd` para teclas.
- `.card` (de `<Card>`): `layer-1`, hairline, `--shadow-1`, raio `--r-lg`; cabeçalho com
  `.card-icon` (28×28, `--accent-soft`, ícone `--accent-text`), título 14/600, descrição 12.5
  `--label-tertiary`; linhas separadas por hairline.
- `.switch`, `.segmented`, `.stat`, `.empty-state`, `.toast`, `.dot` (6px + halo do mesmo tom a 25%),
  `.logo`, `.spinner` (anel girando, para trabalho em andamento).
- Lista/linha selecionável `.list-row`: raio `--r-sm`, hover `--hover`, selecionada `--active` +
  barra de 2px `--accent` à esquerda.

## Superfícies

**Shell (OpenHuman):** moldura `--bg-chrome`; sidebar 264px na moldura; conteúdo num cartão inset
(margem 8px, raio `--r-lg`, hairline, `--bg-base`). Sidebar: marca (ícone + "Zerith"), botão
primário "Nova conversa" (cartão `layer-1` + sombra 1, ícone `SquarePen`, atalho `Ctrl N`), busca com
ícone, conversas agrupadas por data (Hoje / Ontem / 7 dias / Mais antigas) com rótulo de grupo em
caixa-alta 10.5/600, linha com logo do provedor + título + hora; rodapé com Memória (Brain + ponto
de estado) e Ajustes (Settings).

**Conversa (Abyss):** topo com título 15/600, pílula de rota (logo + modelo), pílula de estado
(ponto respirando quando respondendo), botão de painel lateral. Usuário: bolha à direita `layer-2`,
raio 16 com canto inferior direito 6, máx 76%. Assistente: avatar 24px (marca) + markdown
renderizado (títulos, listas, `code`, blocos de código com faixa de linguagem e botão copiar,
citações, links). Raciocínio: dobra "Raciocínio" (Brain). Ferramentas: linhas-cartão compactas —
ícone por ferramenta num quadrado 22px `--accent-soft`, nome 13/600, argumento resumido em mono
`--label-tertiary`, estado à direita (spinner / check `--ok` / x `--danger`), chevron para abrir a
prévia mono. Erro: callout `--danger-soft` com ícone. Nota: pílula central discreta com ícone Info.

**Composer (Abyss):** cartão flutuante `layer-1`, hairline `--line-strong`, `--shadow-composer`,
raio `--r-xl`; foco = borda acento + anel `--accent-soft`. Linha inferior: pílulas (modo com
ShieldCheck, modelo com logo) à esquerda; Parar (Square) e Enviar (círculo 32px acento, ArrowUp,
`--glow-accent` quando pode enviar) à direita. Popup de `/` e `@` = popover `layer-3`, linhas com
ícone. Dica abaixo com `.kbd`.

**Estado vazio da conversa:** herói centralizado — marca 40px, "Como posso ajudar?" (`--fs-hero`,
600, -0.01em), subtítulo, composer, 4 sugestões em cartões com ícone.

**Painel lateral (inspector):** só aparece com conversa aberta; cartões "Tarefas" (CircleDashed /
Loader / CheckCircle2), "Rota" (logo + conta + modelo), "Uso" (tokens entrada/saída em mono,
economizados em `--ok-text`).

**Ajustes (OpenHuman settings):** diálogo `min(1100px, 94vw) × min(740px, 90vh)`, `--shadow-3`,
raio `--r-xl`, backdrop com blur. Nav esquerda `--bg-chrome` com título, grupos em caixa-alta e
itens ícone+rótulo no padrão `.list-row`. Painel: cabeçalho (ícone 32px `--accent-soft` + título
`--fs-h2` + descrição + fechar), corpo rolável com coluna de no máx 760px e cartões. Toast de aviso
no topo do corpo.

## Verificação

`npm run dev` + `bash scripts/shots.sh <saída> <cena[:aba]>` fotografa o app sobre o núcleo falso
(`src/preview`). Cenas: `empty chat approval question settings:<Aba>`. `THEMES=dark` limita a um
tema. Sempre olhe o print antes de dizer que ficou bom; `npx tsc --noEmit` sem erro nos seus
arquivos.
