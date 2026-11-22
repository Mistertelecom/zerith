---
name: proxmox-networking-sdn
description: Redes em Proxmox VE e Linux: Linux Bridges VLAN-aware (vmbr), LACP Bonds, Proxmox SDN (VNets, VLAN, QinQ, VXLAN, EVPN/BGP via FRR) e pve-firewall.
category: devops
---

# Redes Proxmox VE (Bridges VLAN-Aware, LACP, SDN & Firewall)

Use esta skill ao configurar ou diagnosticar conectividade entre o switch físico (Cisco/Huawei/MikroTik) e os nós **Proxmox VE** (`/etc/network/interfaces`, `ifreload -a`, Proxmox SDN e `pve-firewall`).

## 1. Inspeção de Interfaces, Bridges e VLANs no Nó Proxmox

```bash
ip -br link
ip -br addr
ip route show
bridge vlan show
cat /etc/network/interfaces
cat /etc/network/interfaces.d/sdn 2>/dev/null || true
```
- Ao alterar `/etc/network/interfaces`, use **`ifreload -a`** (pacote `ifupdown2` nativo do Proxmox VE) para aplicar mudanças sem derrubar as interfaces ativas, e use `net_diff` para comparar o antes/depois do `/etc/network/interfaces`.

## 2. Boas Práticas de Bridge VLAN-Aware (`vmbr0`) e Bonding (`bond0`)

```text
auto bond0
iface bond0 inet manual
    bond-slaves eno1 eno2
    bond-miimon 100
    bond-mode 802.3ad
    bond-xmit-hash-policy layer2+3

auto vmbr0
iface vmbr0 inet manual
    bridge-ports bond0
    bridge-stp off
    bridge-fd 0
    bridge-vlan-aware yes
    bridge-vids 2-4094
```

## 3. Troubleshooting de `pve-firewall`

- Verificar status nos 3 níveis (Datacenter, Node, VM/CT):
  - `pve-firewall status`
  - `/etc/pve/firewall/cluster.fw`
  - `/etc/pve/firewall/<vmid>.fw`
