---
name: isp-core-services-linux
description: Administração, hardening e tuning de serviços Linux críticos de provedores e empresas: DNS Recursivo/Autoritativo (Unbound, Bind9), FreeRADIUS + SQL (radacct), Accel-PPP, NTP Chrony e firewall nftables.
category: devops
---

# Serviços Críticos Linux (DNS Unbound/Bind9, FreeRADIUS, Accel-PPP & Nftables)

Use esta skill ao administrar ou diagnosticar VMs Linux que rodam os serviços essenciais de provedores e empresas.

## 1. DNS Recursivo de Alta Performance (`Unbound` & `Bind9`)

- **Segurança Obrigatória (Nunca deixar Open Resolver):**
  - Em `unbound.conf`, valide `access-control:` restrito exclusivamente aos blocos IPv4/IPv6 do ASN do cliente, `localhost` e faixas privadas de CGNAT/gerência (`100.64.0.0/10`, `10.0.0.0/8`).
- **Tuning para Alta Taxa de Consultas (QPS):**
  - `num-threads` alinhado ao número de vCPUs da VM; `msg-cache-slabs`, `rrset-cache-slabs`, `infra-cache-slabs`, `key-cache-slabs` em potência de 2; `rrset-cache-size` = 2x `msg-cache-size`; `so-reuseport: yes`; `prefetch: yes`; `serve-expired: yes`.
- **Diagnóstico ao vivo:**
  ```bash
  unbound-control stats_noreset | grep -E "total.num|mem.cache"
  dig @127.0.0.1 google.com +stats
  ```

## 2. FreeRADIUS + Banco SQL (`MySQL` / `MariaDB` / `PostgreSQL`)

- Quando ocorre queda de energia regional ou flap de OLT, milhares de ONUs tentam autenticar PPPoE simultaneamente (**PPPoE Reconnect Storm**):
  - Se o FreeRADIUS demorar `> 100 ms` por query no SQL, a fila estoura (`Dropping request`) e os clientes não reconectam.
  - **Causa raiz clássica:** tabela `radacct` com milhões de linhas históricas sem índice composto em `(username, acctstoptime)` ou `(acctuniqueid)` e sem particionamento/expurgo de sessões antigas.
  - Verifique `SHOW FULL PROCESSLIST;` / `pg_stat_activity`, índices de `radacct`, `radcheck`, `radreply` e pool de conexões em `/etc/freeradius/3.0/mods-available/sql`.
