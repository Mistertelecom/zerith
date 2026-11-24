# Servidor de workflows (`zerith server`)

O desktop desenha o workflow e pergunta "executar em: **este PC** | **servidor X**".
"Servidor X" é qualquer máquina (normalmente Linux) com o Zerith instalado e
pareada com o desktop. Nada de endereço fixo no código: cada servidor é
cadastrado pelo código de pareamento.

## Arquitetura

```
desktop ──HTTPS/VPN──▶ [Caddy | Tailscale | WireGuard] ──HTTP──▶ zerith server (127.0.0.1:7878 ou IP privado)
                                                                 │
                                                                 ├─ <raiz>/workflows.db   (mesmo banco do daemon/desktop)
                                                                 ├─ <raiz>/server/config.toml
                                                                 ├─ <raiz>/server/tokens.json      (só hashes)
                                                                 ├─ <raiz>/server/token_usage.json (último uso)
                                                                 ├─ <raiz>/server/secrets.json     (só nomes)
                                                                 └─ cofre do sistema (valores dos segredos)
```

- Código: `crates/zerith-cli/src/server/` (`http.rs` parser, `auth.rs`
  bind/tokens/limite, `routes.rs` rotas, `events.rs` SSE).
- Cliente e tipos para o desktop: `crates/zerith-workflows/src/remote.rs`
  (`PairingCode`, `RemoteClient`, `Health`, `PutWorkflow`, `RunStarted`).
- Sem dependência nova: `std::net::TcpListener`, uma thread por conexão (máx.
  64), HTTP/1.1 escrito à mão. Limites: cabeçalho 16 KB, corpo 1 MB,
  `Content-Length` obrigatório para corpo (chunked = 501), 15 s para a
  requisição chegar, uma requisição por conexão.
- Execuções disparadas pela API: no máximo 8 ao mesmo tempo, `trigger_source`
  `remote`. Nós `ai_agent` usam a conta/modelo configurados no servidor (uma
  chamada ao modelo, sem o loop de ferramentas por enquanto).
- Dentro do daemon: `server::spawn(&paths) -> Result<JoinHandle<()>, _>` liga o
  servidor configurado e devolve a thread que aceita conexões; o `zerith
  daemon` pode chamar isso antes do laço do agendador (ligação ainda não feita
  em `daemon.rs`).

### Por que não tem TLS

Decisão: o servidor não faz TLS próprio (sem rustls/rcgen/certificados para
manter). O transporte cifrado fica com quem já faz isso bem:

- **Tailscale / WireGuard** (recomendado): o servidor escuta no IP da VPN
  (`100.x.y.z` ou `10.x`); só máquinas da sua rede alcançam.
- **Caddy** como proxy HTTPS: o servidor fica em `127.0.0.1:7878` e o Caddy
  publica `https://zerith.seudominio` com certificado automático.

Por isso o bind é fechado por padrão:

| Endereço | Aceito |
|---|---|
| `127.0.0.1`, `::1` | sempre (padrão `127.0.0.1:7878`) |
| `10/8`, `172.16/12`, `192.168/16`, `100.64/10` (Tailscale/CGNAT), `fd00::/8` | sem flag |
| `0.0.0.0`, `::`, IP público | só com `--allow-public` (fica `allow_public = true` no config) e um aviso forte a cada início |

## Instalação manual (Linux, systemd)

```bash
sudo useradd --system --home /var/lib/zerith --create-home --shell /usr/sbin/nologin zerith
sudo install -m 0755 zerith /usr/local/bin/zerith

# configurar como o usuário zerith, com a raiz dele
sudo -u zerith env ZERITH_HOME=/var/lib/zerith zerith server init --name casa --bind 100.101.102.103:7878
# (contas/modelos para nós ai_agent: sudo -u zerith env ZERITH_HOME=/var/lib/zerith zerith accounts add ...)
```

`/etc/systemd/system/zerith-server.service`:

```ini
[Unit]
Description=Zerith server (API de workflows)
After=network-online.target tailscaled.service
Wants=network-online.target

[Service]
User=zerith
Group=zerith
Environment=ZERITH_HOME=/var/lib/zerith
# Fuso usado por gatilhos cron sem fuso explícito.
Environment=TZ=America/Sao_Paulo
ExecStart=/usr/local/bin/zerith server serve
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/var/lib/zerith
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now zerith-server
journalctl -u zerith-server -f
```

Quando o daemon passar a embutir o servidor (`server::spawn`), troque
`zerith server serve` por `zerith daemon` no `ExecStart`.

> Workflows com nós `shell`/`ssh_command` rodam como o usuário `zerith`, com
> as permissões dele. Não rode o serviço como root.

## Expor para o desktop

**Tailscale**: `tailscale ip -4` no servidor dá o `100.x.y.z`; use
`--bind 100.x.y.z:7878`. O desktop precisa estar na mesma tailnet. Restrinja
com ACLs do Tailscale se quiser só o seu dispositivo.

**WireGuard**: `--bind 10.8.0.1:7878` (IP do servidor no túnel).

**Caddy** (HTTPS público, servidor em loopback):

```caddyfile
zerith.seudominio.com {
    reverse_proxy 127.0.0.1:7878 {
        # SSE: não bufferizar /v1/events
        flush_interval -1
    }
}
```

e no init: `zerith server init --bind 127.0.0.1:7878 --url https://zerith.seudominio.com`.
Atrás de proxy, o limite de falhas por IP vê o IP do proxy (todas as falhas
somam juntas); mantenha o proxy restrito.

## Pareamento

```bash
zerith server init [--name <nome>] [--bind <ip:porta>] [--url <url>] [--device <nome>] [--allow-public]
```

Cria `server/config.toml`, o primeiro token (dispositivo `desktop`, ou
`--device`) e imprime o **código de pareamento** sozinho numa linha e, em
seguida, as instruções. `--url` é o endereço que o desktop usa (padrão
`http://<bind>`; obrigatório com bind `0.0.0.0`).

Formato do código:

```
zerith-pair:<base64url sem padding do JSON {"v":1,"name":"...","url":"http(s)://...","token":"zt_..."}>
```

O código contém o token: trate como senha. Ele não é mostrado de novo — o
servidor só guarda `sha256(token)`. No desktop: Workflows > Servidores >
Adicionar servidor > colar. O desktop chama `GET /v1/ping` (sem token) e
depois `GET /v1/health` (com token).

Outro dispositivo: `zerith server token add <dispositivo>` (novo código).

## Revogação

```bash
zerith server token list                 # dispositivo, criado em, último uso
zerith server token revoke <dispositivo>
```

Vale na hora: cada requisição relê `tokens.json`, sem reiniciar. Perdeu o
notebook? Revogue o dispositivo dele e pareie de novo nos outros se quiser
trocar tudo.

## API (`/v1`, JSON)

Todas exigem `Authorization: Bearer <token>`, menos `GET /v1/ping`. Token
ausente/errado/revogado: 401. Mais de 10 falhas por minuto do mesmo IP: 429
(inclusive com token certo) até a janela esvaziar.

| Método e rota | O que faz |
|---|---|
| `GET /ping` | `{name:"zerith", version}` — sem token |
| `GET /health` | versão, hostname, uptime, `daemon` (status do daemon, se disponível), workflows total/ativos/com falha, execuções ativas |
| `GET /workflows` | lista |
| `GET /workflows/:id` | um workflow |
| `PUT /workflows/:id` | `{workflow, base_updated_at_ms}`; cria (201) ou atualiza (200). 409 com `current` se a versão do servidor difere da base |
| `DELETE /workflows/:id` | apaga (e as execuções) |
| `POST /workflows/:id/enabled` | `{enabled: bool}` |
| `POST /workflows/:id/run` | `{input?}`; dispara em thread, 202 `{run_id}` |
| `GET /workflows/:id/runs?limit=` | execuções recentes (1–200, padrão 20) |
| `GET /runs/:run_id` | execução com logs por nó |
| `GET /secrets` | só os nomes gravados por esta API |
| `PUT /secrets/:nome` | `{value}`; grava no cofre do sistema |
| `DELETE /secrets/:nome` | apaga do cofre |
| `GET /events` | Server-Sent Events: `run_started`, `node`, `run_finished` (com `id:`), e `daemon_status` a cada 30 s. Reconexão com `?since=<id>` ou `Last-Event-ID` (últimos 500 eventos em memória) |

**Concorrência otimista**: `updated_at_ms` é a versão. O desktop guarda o
`updated_at_ms` que recebeu e manda como `base_updated_at_ms`; se outro
dispositivo salvou antes, vem 409 com a versão atual para mesclar ou
sobrescrever conscientemente (novo PUT com a base nova).

**Status do daemon**: `health.daemon` é a última linha da tabela
`daemon_status` em `<raiz>/workflows.db` (todas as colunas, como objeto), ou
`null` se a tabela não existir. O agendador dentro do `zerith daemon` grava
essa linha; quando existir `Store::daemon_status()`, a leitura passa a usá-la.

## O que nunca sai do servidor

- **Valores de segredos**: só entram (`PUT /secrets/:nome`). Nenhuma rota
  devolve valor; `GET /secrets` lista nomes. O valor vai para o cofre do
  sistema (`zerith_core::secrets`), nunca para arquivo em texto puro; o
  índice `server/secrets.json` tem só nomes.
- **Tokens**: só o hash sha256 fica em disco. Respostas e logs nunca ecoam o
  token; o `Debug` de `PairingCode`/`RemoteClient` esconde o token.
- Arquivos de `server/` são gravados com modo `0600` no Unix.

Atenção: os *workflows* em si (comandos, URLs, saídas dos nós) vão e voltam
pela API — não coloque segredos direto no texto do workflow; guarde no cofre.

## Limites conhecidos

- Sem TLS próprio (decisão acima).
- `GET /secrets` só conhece nomes gravados pela API (o cofre do SO não lista
  entradas); segredos gravados com `zerith secret set` não aparecem.
- Limite de falhas é por IP e em memória (zera ao reiniciar).
- Nós `ai_agent` no servidor fazem uma chamada ao modelo, sem ferramentas.
