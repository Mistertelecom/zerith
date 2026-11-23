---
name: proxmox-cluster-ha
description: "Use em clusters Proxmox VE: perda de quorum, corosync (knet, token, ring/link, rede dedicada), QDevice, HA que reinicia ou migra VMs sozinho, VM que perde rede depois de migrar entre nós, memória do host (ZFS ARC, ballooning, OOM killer matando VMs), backup que falha por guest agent/fs-freeze e conntrack cheio no host."
---

# Cluster Proxmox VE: quorum, HA, migração e recursos do host

Sempre leia antes de mexer: `pvecm status`, `pvecm nodes`, `ha-manager status`, `journalctl -u corosync -u pve-cluster --since "-2h"`.

## 1. Quorum e corosync
- O corosync é **sensível a latência**: o token padrão é curto e qualquer atraso derruba o membro. Sintomas: `Token has not been received`, `link: host: X link: 0 is down`, `Members[1]`, "no quorum" por segundos.
- **Causa mais comum: corosync na mesma rede/bridge do tráfego das VMs** (ring0 num IP da `vmbr0`). Backup, migração ou pico de tráfego atrasam o token.
- Correto: rede dedicada para o corosync (porta física ou VLAN só para isso) e um **segundo link** (`ring1_addr` / `link1`) como redundância.
- Mudar o corosync em produção, sem perder quorum:
  1. `pvecm status` com todos os nós e o QDevice OK.
  2. Configure e teste o IP novo nos nós (`ifreload -a`, ping entre eles na rede nova).
  3. Edite **`/etc/pve/corosync.conf`** (nunca o `/etc/corosync/corosync.conf` direto), **incremente `config_version`**, acrescente o novo link sem remover o antigo.
  4. Confirme `corosync-cfgtool -s` com os dois links up; só depois remova o link antigo, de novo incrementando a versão.
- Não aumente `token` para esconder um problema de rede.

## 2. QDevice e cluster de 2 nós
- Dois nós sem QDevice perdem quorum quando um cai. Com QDevice (`corosync-qnetd` num terceiro host), o voto extra mantém o quorum.
- Confira: `pvecm status` (deve listar o Qdevice com voto), `corosync-qdevice-tool -s`.
- O QDevice também precisa de rede estável até os nós.

## 3. HA
- `ha-manager status`, `ha-manager config`. O HA reinicia/move VMs quando um nó perde quorum (fence): quorum instável = HA "sem motivo".
- Resolva o quorum antes de mexer no HA. Para manutenção: `ha-manager crm-command node-maintenance enable <nó>`.

## 4. VM perde rede depois de migrar
- Compare `/etc/network/interfaces` entre os nós: bridges com o **mesmo nome** e, se `bridge-vlan-aware yes`, os **mesmos `bridge-vids`**. Uma VM com `tag=310` só tem rede no nó cujo `bridge-vids` inclui 310.
- Confira também o trunk do switch para cada nó (a VLAN precisa passar nos dois).
- Aplique com `ifreload -a` (ifupdown2), que não derruba as interfaces que não mudaram. Tenha acesso por IPMI/iDRAC antes.

## 5. Memória do host: ZFS ARC e OOM
- Sem `zfs_arc_max`, o ARC pode crescer até **metade da RAM** e disputar com as VMs: o OOM killer do host mata o processo `kvm` da VM maior.
- `zfs_arc_min` sozinho **não limita** o ARC. O que limita é `zfs_arc_max`.
- Ajuste em `/etc/modprobe.d/zfs.conf`:
  ```
  options zfs zfs_arc_max=17179869184
  ```
  Aplique na hora com `echo 17179869184 > /sys/module/zfs/parameters/zfs_arc_max` e deixe persistente com `update-initramfs -u -k all`.
- Confira com `arc_summary` e `free -h`. Some a RAM configurada das VMs + ARC máximo + ~4 GB do host: não pode passar da RAM física.
- Ballooning ajuda pouco com banco de dados: prefira memória fixa para a VM do banco.

## 6. Backup que falha: guest agent e fs-freeze
- `vzdump --mode snapshot` com `agent: 1` pede `fs-freeze` ao QEMU guest agent. Se o agent está desligado na VM ou não roda no sistema convidado, o freeze falha.
- Corrija nos dois lados: `qm set <vmid> --agent enabled=1` **e** instale/ative o `qemu-guest-agent` dentro da VM (Linux: `systemctl enable --now qemu-guest-agent`; Windows: virtio-win).
- Teste: `qm agent <vmid> ping` e `qm guest cmd <vmid> fsfreeze-status`.
- `agent: 1` só vale depois de desligar e ligar a VM (não basta reiniciar o convidado).

## 7. Conntrack no host
- `nf_conntrack: table full, dropping packet` no host derruba conexões das VMs que passam pelo netfilter do host (pve-firewall, bridges com firewall, VM de NAT).
- Veja uso: `sysctl net.netfilter.nf_conntrack_count net.netfilter.nf_conntrack_max`.
- Ajuste em `/etc/sysctl.d/` (ex.: `net.netfilter.nf_conntrack_max = 1048576`) e aplique com `sysctl --system`. Ajuste também `nf_conntrack_buckets` se a tabela crescer muito.
