# Monitoramento: equipamentos offline

Modelos prontos (Workflows > Novo > modelos) que, num horário, listam o que
está fora do ar e avisam um grupo. Rodam no desktop ou num servidor
(`zerith daemon`, veja `docs/servidor.md`). Todos têm o mesmo formato:

```
Todo dia 07:50 → consulta → Montar lista de offline → Há offline? → Avisar o grupo
```

- **Gatilho**: cron `50 7 * * *` (troque à vontade; fuso do servidor ou o do nó).
- **Montar lista de offline**: Transformar no modo `offline_report`. Gera
  `Equipamentos offline às HH:MM (N):` e uma linha por equipamento, ou
  `Nenhum equipamento offline às HH:MM`.
- **Há offline?**: só deixa passar quando há algo offline. Apague o nó (ligue
  a lista direto no aviso) para receber o "nenhum offline" também.
- **Avisar o grupo**: escolha o canal (Evolution API etc., `docs/whatsapp.md`)
  e o destino (`…@g.us` para grupo).
- **Aviso de falha** ligado (1 falha): se a consulta quebrar (senha errada,
  equipamento fora), o canal padrão de alertas do servidor recebe o erro.

## The Dude (RouterOS com o pacote `dude`)

Nó **SSH** no RouterOS que roda o servidor do Dude:

```
/dude device print detail without-paging
```

Preencha no nó SSH:

| Campo | O quê |
|---|---|
| `device` | `cliente/nome` do inventário (`<raiz>/inventory.toml`): pega host, porta, usuário, senha (`secret_ref`) e `legacy_ssh` |
| ou `host`, `user`, `port` | direto no nó |
| `password_secret` | nome do segredo com a senha (grave no servidor pela tela ou `zerith secret set <nome>`) |
| `legacy_ssh` | RouterOS antigo (ssh-rsa / DH group14-sha1) |

Crie no RouterOS um usuário só leitura para isso (`/user add group=read`).

### Status do dispositivo pelo CLI — leia antes

O CLI do Dude é pouco documentado. A referência oficial
(manual.mikrotik.com, `/dude device`) lista só `name`; no fórum há relatos de
`print detail` mostrando apenas o nome e de filtros por status
(`where .dead`) que não funcionam. Por isso o modelo **não** filtra no
RouterOS: busca `print detail` inteiro e o Transformar filtra.

O Transformar lê o formato `print detail` do RouterOS (`chave=valor`, com
registros por índice) e considera offline quem tem `status_field` (padrão
`status`) com um dos `down_values` (padrão `down`, separados por vírgula). O
nome sai de `name` e o endereço de `address`/`addresses`/`ip`.

Se no seu RouterOS o campo tiver outro nome, ajuste `status_field`/
`down_values` no nó. Se o `print detail` não trouxer status nenhum, a lista
sempre sairá vazia — use o Zabbix ou uma notificação do próprio Dude.
Teste antes: no nó SSH, **Testar nó** mostra a saída real.

## Zabbix

Veja `docs/zabbix.md`.
