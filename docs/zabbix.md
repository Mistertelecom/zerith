# Zabbix nos workflows

Nó **Zabbix** (categoria Coleta & Ação) fala com a API JSON-RPC do Zabbix.
Alvo: **Zabbix 7.0 LTS** (vale do 5.4 em diante com token de API).

## Configuração do nó

| Campo | O quê |
|---|---|
| `url` | `https://zabbix.exemplo/zabbix` (o nó acrescenta `/api_jsonrpc.php`; URL terminando em `.php` vai como está) |
| `token_secret` | nome do segredo com o **token de API** (Zabbix: Usuários > Tokens de API). Também aceita `token: "{{secret:nome}}"`; texto puro é recusado |
| `auth_mode` | `header` (padrão): `Authorization: Bearer <token>`, o jeito do 6.4+/7.x. `auth_field`: campo `auth` no corpo, para versões antigas (no 7.0 está obsoleto) |
| `operation` | `hosts_unavailable`, `problems` ou `raw` |
| `groups` | nomes de grupos de hosts, separados por vírgula (opcional; `hostgroup.get` acha os ids) |
| `severities` / `min_severity` | só `problems`: lista (`[3,4,5]`) ou mínimo (`3` = Média) |
| `patterns` | só `hosts_unavailable`: trechos do nome do problema, separados por `|` (padrão `Unavailable by ICMP ping|is not available`) |
| `method`, `params` | só `raw`: método e params em JSON (aceita `{{prev}}`) |
| `timeout_secs` | padrão 30 |
| `insecure_tls` | aceita certificado próprio (desligado por padrão; use só na rede interna) |

Crie no Zabbix um usuário só leitura (papel "User" com acesso de leitura aos
grupos) e gere o token para ele. Grave o token no servidor:

```bash
sudo -u zerith env ZERITH_HOME=/var/lib/zerith ZERITH_SECRETS_BACKEND=file zerith secret set zabbix-token
```

ou pela tela do desktop (inspetor do nó > Definir no servidor).

## Operações

**`problems`** — três chamadas, porque no 7.0 o `problem.get` não aceita
`selectHosts` (`Invalid parameter "/": unexpected parameter "selectHosts"`):

1. `problem.get` com `output: [eventid, objectid, name, severity, clock]`,
   `recent: false`, `suppressed: false` (e `groupids`/`severities`);
2. `trigger.get` com `triggerids` = os `objectid` e `selectHosts: [hostid, host]`;
3. `host.get` com `selectInterfaces: [ip, type, main, available]` para o IP
   (interface principal, agente antes de SNMP).

**`hosts_unavailable`** — o jeito mais confiável no 7.0:

- problemas ativos cujo nome casa com `patterns` (o template "ICMP Ping" gera
  `Unavailable by ICMP ping`; o do agente, `Zabbix agent is not available`);
- mais hosts monitorados com alguma interface `available = 2` (indisponível:
  agente, SNMP, IPMI, JMX), com o `error` e o `errors_from` da interface.

Não dá para depender só de `available`: no 7.0 ele é por interface
(0 desconhecido, 1 disponível, 2 indisponível) e um host monitorado só por
ICMP fica com 0 mesmo fora do ar. Um item por host.

**`raw`** — devolve o `result` da chamada, em JSON.

Saída de `problems`/`hosts_unavailable` (JSON, para o Transformar
`offline_report` montar a mensagem):

```json
[{"host": "olt-01", "ip": "192.0.2.10", "problema": "ICMP Ping: Unavailable by ICMP ping",
  "severidade": "Alta", "desde": 1795567712}]
```

Erro do JSON-RPC (`error.message` + `error.data`) vira falha do nó com a
mensagem do Zabbix; o token nunca aparece (é trocado por `***`).

## Como testar sem um Zabbix em produção

Zabbix 7.0 mínimo em Docker (só para teste; troque as senhas):

```yaml
# docker-compose.yml
services:
  postgres:
    image: postgres:16-alpine
    environment:
      POSTGRES_USER: zabbix
      POSTGRES_PASSWORD: zabbix-teste
      POSTGRES_DB: zabbix
    volumes: [pgdata:/var/lib/postgresql/data]
  zabbix-server:
    image: zabbix/zabbix-server-pgsql:alpine-7.0-latest
    environment:
      DB_SERVER_HOST: postgres
      POSTGRES_USER: zabbix
      POSTGRES_PASSWORD: zabbix-teste
      POSTGRES_DB: zabbix
    depends_on: [postgres]
    ports: ["10051:10051"]
  zabbix-web:
    image: zabbix/zabbix-web-nginx-pgsql:alpine-7.0-latest
    environment:
      ZBX_SERVER_HOST: zabbix-server
      DB_SERVER_HOST: postgres
      POSTGRES_USER: zabbix
      POSTGRES_PASSWORD: zabbix-teste
      POSTGRES_DB: zabbix
      PHP_TZ: America/Sao_Paulo
    depends_on: [zabbix-server]
    ports: ["8080:8080"]
volumes:
  pgdata:
```

1. `docker compose up -d`; espere ~1 min e abra `http://localhost:8080`
   (login inicial `Admin` / `zabbix` — troque).
2. Usuários > Tokens de API > Criar token; copie o valor.
3. Crie um host de teste com o template **ICMP Ping** apontando para um IP
   que não responde (ex. `192.0.2.10`, faixa de documentação): em ~3 min
   aparece o problema `Unavailable by ICMP ping`.
4. Confira a API à mão:
   ```bash
   curl -s -H 'Content-Type: application/json-rpc' -H "Authorization: Bearer $TOKEN" \
     -d '{"jsonrpc":"2.0","method":"problem.get","params":{"output":["eventid","objectid","name","severity","clock"],"recent":false},"id":1}' \
     http://localhost:8080/api_jsonrpc.php
   ```
5. No Zerith: `zerith secret set zabbix-token`, abra o modelo, ponha
   `url = http://localhost:8080` e **Testar nó** no nó Zabbix.

Os testes automáticos (`cargo test -p zerith-workflows zabbix`) usam um
servidor JSON-RPC falso com respostas no formato do 7.0; nunca falam com um
Zabbix de verdade.
