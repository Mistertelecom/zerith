# Skills que vêm com o Zerith

Estas skills são conteúdo de terceiros, copiado sem alterações. O Zerith não
as escreveu. Cada uma mantém a licença do autor, que você pode ler junto dela.

Elas entram **desativadas e não revisadas**: nada é executado até você ativar
uma skill na tela Skills depois de ver o que ela contém.

| Skill | Origem | Commit | Licença |
|---|---|---|---|
| skill-creator, mcp-builder, webapp-testing, frontend-design, docx, pdf, pptx, xlsx | https://github.com/anthropics/skills (pasta `skills/<nome>`) | e4bb5d9 | Apache-2.0, arquivo `LICENSE.txt` dentro de cada skill |
| cloudflare, cloudflare-one, workers-best-practices, turnstile-spin | https://github.com/cloudflare/skills (pasta `skills/<nome>`) | cbf893b | Apache-2.0, ver `LICENSE.cloudflare` |
| agent-desktop, agent-desktop-windows | https://github.com/vercel-labs/agent-desktop (pasta `skills/<nome>`) | 8ffecc1 | Apache-2.0, ver `LICENSE.vercel-agent-desktop` |

## O que ficou de fora e por quê

- `doc-coauthoring` (anthropics/skills): a skill não traz licença e o repositório
  não declara uma para o conjunto. Sem licença explícita não é permitido
  redistribuir.
- `computer use` do Claude Code: não existe como skill publicada, e o código do
  Claude Code é "todos os direitos reservados". Usamos `agent-desktop`, que faz
  a mesma tarefa e tem licença aberta.

## Computer use precisa de um programa à parte

A skill `agent-desktop` ensina o modelo a usar o programa `agent-desktop`
(CLI de acessibilidade, Apache-2.0). O Zerith **não** o inclui: instale com
`npm install -g agent-desktop` ou baixe de
https://github.com/vercel-labs/agent-desktop/releases. Sem ele a skill não faz
nada, e o modelo deve avisar isso.
