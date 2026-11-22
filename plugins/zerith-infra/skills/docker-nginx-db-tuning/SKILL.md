---
name: docker-nginx-db-tuning
description: Troubleshooting e tuning de produção para Docker/Compose, Kubernetes/K3s, proxies reversos (Nginx, Traefik, HAProxy), bancos de dados (PostgreSQL, MariaDB, Redis) e parâmetros de kernel Linux (sysctl.conf).
category: devops
---

# Docker, Nginx/Traefik, Bancos de Dados & Kernel Tuning (`sysctl`)

Use esta skill em servidores Linux de aplicação, bancos de dados e containers de produção.

## 1. Diagnóstico Rápido de Gargalo em Servidor Linux

```bash
uptime && uname -r
df -hT && df -i
free -h && vmstat 1 3
ss -s
dmesg -T | tail -n 30
journalctl -p 3 -xb --no-pager | tail -n 30
```

## 2. Tuning de Kernel Linux para Alto Tráfego (`sysctl`)

Em servidores DNS, proxies reversos, gateways Linux ou nós Docker sob alta carga, verifique:
- **Tabela Conntrack cheia (`nf_conntrack: table full, dropping packet` no `dmesg`):**
  - `sysctl net.netfilter.nf_conntrack_count net.netfilter.nf_conntrack_max`
  - Elevar `net.netfilter.nf_conntrack_max = 1048576` em `/etc/sysctl.d/99-performance.conf`.
- **Fila de Sockets e Portas Efêmeras:**
  - `net.core.somaxconn = 16384`
  - `net.ipv4.ip_local_port_range = 10240 65535`
  - `net.core.rmem_max = 16777216` / `net.core.wmem_max = 16777216`

## 3. Containers Docker & Proxies Reversos (`Nginx` / `Traefik` / `HAProxy`)

- `docker ps -a` e `docker stats --no-stream`
- Validar configuração antes de reload: `nginx -t && systemctl reload nginx`
