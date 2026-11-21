---
name: proxmox-ve-ops
description: Operação e troubleshooting de clusters Proxmox VE, VMs KVM/QEMU (qm), containers LXC (pct), API interna (pvesh), Cloud-Init, alta disponibilidade (ha-manager) e migração online.
category: devops
---

# Operação & Troubleshooting de Proxmox VE (KVM, LXC & Cluster)

Use esta skill sempre que administrar ou diagnosticar servidores **Proxmox VE (PVE)** e **VMs/Containers Linux** de clientes.

## 1. Diagnóstico Rápido de Nó e Cluster Proxmox

Execute em lote via `terminal_batch` ou `net_broadcast` (nos nós do cluster):
```bash
pveversion -v
pvecm status
ha-manager status
pvesm status
qm list
pct list
systemctl --failed
```

## 2. Gerenciamento de VMs KVM (`qm`) e Containers LXC (`pct`)

- **Inspecionar configuração e locks de uma VM/CT:**
  - `qm config <vmid>` / `pct config <ctid>`
  - Se uma VM/CT estiver travada por backup/migração interrompida, verifique se o processo `vzdump`/`qmigrate` ainda existe (`ps aux | grep <vmid>`) antes de destravar com `qm unlock <vmid>` ou `pct unlock <ctid>`.
- **Snapshots Seguros antes de Mudança na VM:**
  - `qm snapshot <vmid> pre-manutencao --description "Antes da alteracao"`
  - Rollback: `qm rollback <vmid> pre-manutencao`
- **Cloud-Init e Templates Linux:**
  - Redimensionar disco: `qm resize <vmid> scsi0 +20G`
  - Configurar IP/Gateway via Cloud-Init: `qm set <vmid> --ipconfig0 ip=10.0.0.50/24,gw=10.0.0.1`
  - Verificar agente QEMU Guest Agent: `qm agent <vmid> ping` e `qm agent <vmid> network-get-interfaces`

## 3. API Interna (`pvesh`) sem Precisar de Scripts Externos

- Consultar recursos do cluster em JSON:
  - `pvesh get /cluster/resources --output-format json`
  - `pvesh get /nodes/<node>/status --output-format json`
  - `pvesh get /nodes/<node>/qemu/<vmid>/status/current --output-format json`
