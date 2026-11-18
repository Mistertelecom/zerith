# Geração de imagem e vídeo

O usuário escolhe **um modelo de imagem** e **um de vídeo** (OpenRouter). Quando
pede uma imagem ou vídeo no chat, a IA chama `generate_image` / `generate_video`:
ela escreve só o **prompt**; modelo e todos os parâmetros (proporção, resolução,
duração, áudio...) vêm das configurações e **a IA não consegue mudá-los**.

> **Custa dinheiro.** Cada chamada é cobrada na sua conta do OpenRouter (imagem:
> centavos; vídeo: de centavos a alguns dólares conforme modelo, duração e
> resolução). As ferramentas pedem aprovação (ação `network`) a cada uso. Defina
> um limite de gasto na chave em <https://openrouter.ai/settings/keys>.
> Modelos `:free` têm limite de 20 req/min.

## Chave

1. Crie uma chave em <https://openrouter.ai/settings/keys>.
2. `zerith media key set` (lê da entrada padrão; ou `zerith media key set <valor>`).
   Fica no cofre do sistema, entrada `openrouter`. Nunca vai para arquivo nem log.
3. `zerith media key check` mostra rótulo e gasto.

Ordem de busca: variável `OPENROUTER_API_KEY`, depois o cofre. A `ZERITH_API_KEY`
(chave do provedor de **chat**) **não** é usada aqui, de propósito.

## Configuração

```toml
[media]
base_url = ""      # opcional: gateway no lugar de https://openrouter.ai/api/v1
output_dir = ""    # padrão: <pasta de dados>/media

[media.image]
model = "bytedance-seed/seedream-5-5"
[media.image.params]      # fixos; a IA não altera
aspect_ratio = "16:9"
resolution = "2K"
n = 1

[media.video]
model = "google/gemini-omni-1.2-flash"
[media.video.params]
duration = 5
resolution = "720p"
aspect_ratio = "16:9"
generate_audio = false
```

Sem `model`, a ferramenta correspondente **não é oferecida** ao modelo de chat.
A configuração é lida quando o `Registry` é criado (camada do usuário + variáveis
de ambiente); mudanças valem a partir do próximo registry/sessão.
Valores são validados na hora da chamada contra o catálogo do modelo (a lista pode
estar offline na hora de salvar). Parâmetro que o modelo não suporta é **descartado**
e o resultado da ferramenta avisa.

## CLI

```
zerith media show
zerith media models image|video [--query Q] [--limit N] [--offset N]
zerith media set image|video --model ID [--param k=v ...] [--clear-params]
zerith media key set [valor] | check | delete
zerith media generate image|video "<prompt>" [--ref img] [--first-frame img] [--last-frame img]
```

`--param` declara o conjunto completo de parâmetros do slot. `set` valida contra o
catálogo público e recusa valor inválido antes de gravar. `generate` serve para
testar o encanamento sem o chat (**gasta de verdade**).

## Ferramentas

| Ferramenta | Argumentos | Permissão | Limite da chamada |
|---|---|---|---|
| `generate_image` | `prompt`, `reference_images?`, `filename_hint?` | rede (pergunta) | 330 s |
| `generate_video` | `prompt`, `first_frame?`, `last_frame?`, `filename_hint?` | rede (pergunta) | 900 s (polling de até 10 min + envio + download) |

- Argumento desconhecido (`model`, `n`, `duration`...) é rejeitado.
- **Porta de qualidade:** prompt com menos de 12 palavras (imagem) / 18 (vídeo) é
  recusado com a lista do que cobrir (sujeito, cenário, enquadramento, estilo,
  iluminação, paleta, clima, lente, qualidade, o que evitar; vídeo: movimento,
  câmera, ritmo, áudio). Constantes `MIN_WORDS_IMAGE` / `MIN_WORDS_VIDEO` em
  `crates/zerith-tools/src/media.rs`. A ferramenta só devolve o erro; limitar
  as tentativas é papel do laço do agente.
- Arquivos: `<output_dir>/<sessão ou data>/<hint>-<epoch>.<ext>`, sem sobrescrever.
  `filename_hint` vira `[a-z0-9-]`, até 40 caracteres: não escapa da pasta.
- Imagem: resposta limitada a 50 MB; vídeo a 500 MB.
- O resultado traz caminho, modelo, parâmetros **realmente enviados**, avisos,
  custo (se informado) e, para imagem, `![descrição](caminho)`.
- Referências locais só valem dentro do workspace ou da pasta de mídia (não dá para
  mandar `~/.ssh/...` a terceiros) e o conteúdo precisa ser png/jpeg/webp/gif.

## O que foi verificado e o que não foi

**Verificado contra os endpoints públicos reais (2026-11-16, sem chave):**
`GET /images/models` (68 modelos) e `GET /videos/models` (32) com descritores
tipados; `GET /models?output_modalities=image` (preço por imagem/token). O teste
`live_public_lists_parse` (`cargo test -p zerith-providers --test media -- --ignored`)
repete isso.

**Implementado conforme a documentação, mas NÃO exercitado com chamada paga:**
`POST /images` (corpo e resposta `b64_json`), `POST /videos` (202 `{id}`),
polling `GET /videos/{id}` e status, `GET /videos/{id}/content`, `GET /key`
(campos `label`, `usage`, `limit`) e o mapeamento de erros 401/402/403/429. Os
testes usam um servidor falso local que segue essa descrição; se a API real
divergir, o erro será `resposta inesperada`, nunca pânico.

**Não verificado / não feito:**
- Se `/images/generations` é alias de `/images` (não usado).
- Caminho `chat/completions` com `modalities` (alguns modelos Gemini/GPT): **não
  implementado**; modelo que só funcione assim gera erro claro dizendo isso.
- `price_hint` é estimativa: as chaves de preço de vídeo não são uniformes e o
  hint usa a menor tarifa por segundo reconhecível. Nunca é autoritativo.
- Busca de modelos é feita no cliente (as listas têm dezenas de itens); não há
  parâmetro de busca documentado nos endpoints tipados.
- Cancelar durante o polling interrompe o acompanhamento, **não** cancela o job no
  provedor (não há chamada de cancelamento implementada): pode ser cobrado.
- Vídeo com chave ZDR recebe 400 do provedor; a mensagem explica.
- O 9Router não é usado para mídia (só conecta com chave e não filtra
  modalidade); use `base_url` se quiser passar por um gateway.
