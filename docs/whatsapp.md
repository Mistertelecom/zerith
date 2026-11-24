# WhatsApp nos workflows

Os workflows mandam avisos (alerta de falha, resultado de um nó "Notificar
canal") por WhatsApp de três jeitos. Cadastre o canal em **Automações → Canais**.

| Provedor | Grupos | Arquivo | Oficial | Custo |
|---|---|---|---|---|
| Evolution API v2 | sim | sim | não | grátis (você hospeda) |
| WAHA | sim | sim (sendFile, plano Plus para alguns motores) | não | grátis/Plus |
| WhatsApp Cloud API (Meta) | **não** | só texto no Zerith | sim | por conversa |

## Evolution API (recomendado para grupo da empresa)

Projeto: <https://github.com/EvolutionAPI/evolution-api> (Apache-2.0).
O Zerith segue as rotas da v2 (conferido na v2.3.8):
`POST /message/sendText/{instância}`, `POST /message/sendMedia/{instância}`,
`GET /instance/connectionState/{instância}`,
`GET /group/fetchAllGroups/{instância}?getParticipants=false`, todas com o
header `apikey`.

### Subir com docker compose (mínimo)

Escute só em `127.0.0.1` (o Zerith na mesma máquina) ou no IP do WireGuard
(ex.: `10.8.0.1`). Não exponha a porta 8080 na internet.

```yaml
# docker-compose.yml
services:
  evolution:
    image: evoapicloud/evolution-api:v2.3.8
    restart: always
    ports:
      - "127.0.0.1:8080:8080"   # ou "10.8.0.1:8080:8080" (WireGuard)
    environment:
      SERVER_URL: http://127.0.0.1:8080
      AUTHENTICATION_API_KEY: troque-por-uma-chave-longa-aleatoria
      DATABASE_PROVIDER: postgresql
      DATABASE_CONNECTION_URI: postgresql://evo:evo@postgres:5432/evolution?schema=evolution_api
      CACHE_REDIS_ENABLED: "false"
      CACHE_LOCAL_ENABLED: "true"
    volumes:
      - evolution_instances:/evolution/instances
    depends_on: [postgres]
  postgres:
    image: postgres:15
    restart: always
    environment:
      POSTGRES_DB: evolution
      POSTGRES_USER: evo
      POSTGRES_PASSWORD: evo
    volumes:
      - evolution_pg:/var/lib/postgresql/data
volumes:
  evolution_instances:
  evolution_pg:
```

`docker compose up -d`. O compose oficial do repositório traz também Redis e o
painel `evolution-manager`; para só mandar avisos não precisa.

### Criar a instância e ler o QR

```sh
KEY=troque-por-uma-chave-longa-aleatoria
curl -X POST http://127.0.0.1:8080/instance/create \
  -H "apikey: $KEY" -H "Content-Type: application/json" \
  -d '{"instanceName":"empresa","integration":"WHATSAPP-BAILEYS","qrcode":true}'
```

A resposta traz o QR em base64 (`qrcode.base64`, cole num navegador como
`data:image/png;base64,...`) ou abra o painel em `http://127.0.0.1:8080/manager`.
No celular do número dedicado: **Configurações → Aparelhos conectados →
Conectar aparelho**. Se o QR expirar: `GET /instance/connect/empresa`.

### apikey global vs. de instância

- **Global** (`AUTHENTICATION_API_KEY`): manda em tudo (cria e apaga
  instâncias). Use só para administrar.
- **De instância** (o `hash`/`token` devolvido no `instance/create`): só
  envia e lê aquela instância. **Use esta no Zerith.**

### No Zerith

Canal **Evolution API v2**: Base URL (`http://127.0.0.1:8080`), Instância
(`empresa`), API Key (a da instância). **Testar conexão** deve mostrar
`open`. **Escolher grupo** lista os grupos em que o número está; o destino
fica como `120363…@g.us`. Também aceita número: `11999999999` vira
`5511999999999` (o 55 entra quando há 10 ou 11 dígitos sem `+`).

Opcionais no JSON do canal: `delay` (ms, a Evolution mostra "digitando"),
`link_preview` (true/false), `timeout_secs`.

## WAHA

Projeto: <https://github.com/devlikeapro/waha>. Canal **WAHA**: Base URL,
sessão (`default`), API Key (`X-Api-Key`, se configurada). Testar conexão
consulta `GET /api/sessions/{sessão}` e espera `WORKING`. Grupos: cole o
`…@g.us` no destino. Arquivo vai por `POST /api/sendFile`.

```sh
docker run -d -p 127.0.0.1:3000:3000 -e WAHA_API_KEY=troque devlikeapro/waha
```

## WhatsApp Cloud API (Meta, oficial)

- Precisa de app na Meta, número verificado, `phone_number_id` e token
  permanente (usuário do sistema).
- **Não envia para grupos**, só para números.
- Fora da janela de 24 h desde a última mensagem do contato, a Meta só aceita
  **template aprovado**. Crie um template de categoria "Utilidade" com uma
  variável no corpo, ex.: `Alerta do Zerith: {{1}}`, e preencha
  `template_name` / `template_language` (`pt_BR`) no canal. O texto do aviso
  vai na variável (sem quebras de linha). Template sem variável:
  `template_no_params: true`.
- Versão do Graph: `graph_version` (padrão `v26.0`). Troque quando a Meta
  aposentar a versão (cada uma dura cerca de 2 anos).

## Riscos do protocolo não oficial (Evolution, WAHA)

- Usam o protocolo do WhatsApp Web sem autorização da Meta: o número **pode
  ser banido**, sem aviso nem recurso. Use um **número dedicado** (chip só
  para isso), nunca o pessoal ou o do atendimento.
- Mande pouco e só para quem espera a mensagem (grupo interno). Disparo em
  massa ou para desconhecidos é o caminho mais curto para o banimento.
- A sessão cai quando o celular fica muito tempo offline ou o WhatsApp força
  atualização; o **Testar conexão** e o alerta do daemon mostram isso.
- A apikey dá controle total do número: guarde fora de repositório, deixe a
  API só em `127.0.0.1`/WireGuard. O Zerith tira a apikey das mensagens de
  erro, mas o canal fica salvo em `workflows.db`.
- Para algo crítico e auditável, prefira a Cloud API (oficial) ou Telegram.
