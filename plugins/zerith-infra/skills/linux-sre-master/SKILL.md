---
name: linux-sre-master
description: "Use em servidores Linux de ISP/NOC (roteamento, CGNAT, DNS, RADIUS, VPN, NGINX/Docker, FRR/BIRD): triagem determinística em chamada única com net_audit(action='linux_sre_audit'), análise de PSI (cpu/memory/io), ip -j e policy routing (ip rule), tuning de nftables/conntrack, WireGuard e dead-man's switch com systemd-run."
---

# Linux SRE Master (ISP / NOC)

Evite dezenas de idas e voltas no SSH: colete o estado completo do servidor em **uma única chamada read-only** e passe o resultado pelo analisador determinístico.

## 1. Triagem em chamada única (`net_audit` + `terminal_batch`)
1. Obtenha o comando composto read-only:
   `net_audit(action="linux_sre_audit", mode="commands")`
2. Execute o `composite_command` via `net_connect` + `terminal_batch` (ele coleta `hostname`/`uname`/`uptime`, PSI, `free -m`, `df -h`, `systemctl --failed`, `journalctl -p 3`, `ip -j addr/route/rule`, `ss -tulpen`, `conntrack -S`, `wg show all dump` e `vtysh`).
3. Analise imediatamente:
   `net_audit(action="linux_sre_audit", mode="parse", output=...)`

## 2. PSI (Pressure Stall Information)
`/proc/pressure/{cpu,memory,io}` mede o tempo real em que tarefas ficaram **travadas esperando recurso** (muito melhor que `load average` isolado):
- `some avg10 >= 10%`: contenção perceptível; `>= 25%` ou `full avg10 >= 5%`: degradação severa de latência/serviço.
- Se `io` estiver alto com CPU ociosa, investigue disco (`iostat -xz 1 3`, ZFS/NVMe). Se `memory` estiver alto, confira `free -m` (`available`), swap e risco de OOM killer no `journalctl -k`.

## 3. Rede, Policy Routing e CGNAT (`ip -j`, `nftables`, `conntrack`)
- **Roteamento e PBR:** sempre inspecione `ip -j route` e `ip -j rule` juntos (roteamento assimétrico e VPNs com tabelas separadas costumam quebrar por regra em `ip rule`).
- **Conntrack em ISP/CGNAT/Borda:**
  - Confira `conntrack -S`: qualquer valor > 0 em `insert_failed`, `drop` ou `early_drop` significa perda real de sessões!
  - Ajuste `/etc/sysctl.d/99-isp-conntrack.conf`: aumente `net.netfilter.nf_conntrack_max` (ex.: `1048576` conforme RAM) e `nf_conntrack_buckets` (`max / 4`).
  - Em roteadores Linux puros (FRR/BIRD), use `nftables` com `notrack` na chain `raw`/`prerouting` para tráfego de trânsito BGP (porta 179 e blocos roteados que não fazem NAT).

## 4. FRR / BIRD e WireGuard
- **FRR:** `vtysh -c "show ip bgp summary"`, `vtysh -c "show zebra router"`, `systemctl status frr`.
- **WireGuard:** `wg show all dump` — verifique `latest handshake` (`> 180s` indica túnel morto ou NAT/UDP bloqueado) e `AllowedIPs` sobrepostos entre clientes.

## 5. Mudança segura e Dead-Man's Switch (`systemd-run`)
- Valide sempre a sintaxe antes de recarregar:
  `sshd -t`, `nginx -t`, `nft -c -f /etc/nftables.conf`, `ifreload -a -s` ou `netplan try`.
- Antes de aplicar firewall (`nftables`/`iptables`) ou rede remotamente, gere o wrapper de rollback automático:
  `net_audit(action="linux_sre_audit", mode="plan_change", revert_seconds=90)`
- O comando usa `systemd-run --unit=zerith-rollback --on-active=90s ...`: se você perder o SSH, o systemd restaura a configuração anterior automaticamente; se validar o acesso, desarme com `systemctl stop zerith-rollback.timer zerith-rollback.service`.
- Backup/Diff: registre arquivos críticos antes e depois com `net_backup` / `net_diff` ou cópia versionada.
