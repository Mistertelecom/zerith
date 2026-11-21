---
name: proxmox-storage-pbs
description: Diagnóstico e administração de storage Proxmox VE (ZFS, Ceph, LVM-Thin, NFS/iSCSI) e Proxmox Backup Server (PBS, vzdump, prune, verify, garbage collection e restore).
category: devops
---

# Storage Proxmox (ZFS, Ceph, LVM-Thin) & Proxmox Backup Server (PBS)

Use esta skill para diagnosticar degradação de disco, pools ZFS/Ceph, pressão de memória ARC e falhas de backup/restore no **Proxmox VE** e **Proxmox Backup Server (PBS)**.

## 1. Saúde de ZFS e Discos Físicos (SMART / NVMe / SAS)

```bash
zpool status -v
zpool list -v
zfs list -o name,used,avail,refer,mountpoint,compression
arc_summary | head -n 40
lsblk -o NAME,SIZE,TYPE,FSTYPE,MOUNTPOINT,MODEL,SERIAL
```
- **Limitar uso de RAM do ZFS ARC** em nós com memória disputada por VMs:
  - Verificar `/etc/modprobe.d/zfs.conf` (`options zfs zfs_arc_max=...`) e atualizar `update-initramfs -u`.

## 2. Saúde de Cluster Ceph Hiperconvergente

```bash
ceph -s
ceph osd tree
ceph osd df
ceph health detail
```

## 3. Proxmox Backup Server (PBS) e `vzdump`

- No servidor PBS:
  ```bash
  proxmox-backup-manager datastore list
  proxmox-backup-manager task list
  proxmox-backup-manager cert info
  ```
- No nó Proxmox VE:
  - Testar repositório PBS: `pvesm status --storage <pbs-storage>`
  - Disparar backup manual: `vzdump <vmid> --storage <pbs-storage> --mode snapshot --compress zstd`
