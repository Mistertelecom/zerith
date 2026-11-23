---
name: mudanca-segura
description: "Use antes de alterar configuração de roteador, switch, OLT, servidor Linux ou Proxmox, em produção, laboratório ou troubleshooting (corrigir defeito também é mudança): janela, GMUD/MOP, ACL, firewall, VLAN, rota, OSPF, BGP, interface, bridge, /etc/network/interfaces. Backup antes e depois, gerência protegida, rollback e pós-check."
---

# Mudança segura

Nenhuma alteração em produção sem **backup**, **gerência protegida** e **rollback pronto**.

## 1. Backup antes
- Rode `net_backup` no equipamento (ou `net_backup(client=...)` para todos os terminais abertos do cliente) **antes do primeiro comando de configuração**. Sem `net_backup` (acesso por outro script ou jump host), grave a saída de `show running-config` / `/export` / `cat` dos arquivos num arquivo do workspace antes de mudar: é ele que vira o diff e o rollback.
- No Proxmox, faça snapshot da VM antes de mexer nela (`qm snapshot <vmid> pre-mudanca`) e copie o arquivo de rede antes de editar (`cp /etc/network/interfaces /root/interfaces.pre-mudanca`).
- Colete também o estado que a mudança pode afetar: sessões BGP/OSPF, rotas dos prefixos envolvidos, ONUs online na PON e tráfego no uplink.

## 2. Entenda por onde você está conectado (link de gerência)
Antes de mexer em interface, VLAN, ACL, firewall, rota ou bridge, descubra **qual caminho leva a sua sessão**:
1. Pegue o IP de acesso no inventário (`net_inventory`) e veja em qual interface ou VLAN ele está:
   - Cisco: `show ip interface brief`, `show ip route <ip-de-origem>`
   - Huawei: `display ip interface brief`, `display ip routing-table <ip-de-origem>`
   - MikroTik: `/ip address print`, `/ip route print where dst-address in <ip>`
   - Linux e Proxmox: `ip -br addr`, `ip route get <ip-de-origem>`
   - OLT: interface ou VLAN de gerência (in-band no uplink ou porta out-of-band)
2. Se a mudança toca esse caminho, ela é **de risco de perda de acesso**. Avise o usuário explicitamente antes de executar.

## 3. Garanta um segundo acesso antes da mudança de risco
Antes de alterar o caminho da gerência, confirme que existe **outro jeito de entrar** e teste-o:
- Porta de console ou serial (`transport = "serial"` no inventário), porta out-of-band, outro IP de gerência, jump host (`jump_host`) ou acesso pelo IPMI/iDRAC do servidor.
- Se não houver segundo acesso, diga isso ao usuário e proponha um antes: VLAN ou IP de gerência alternativo, ou executar no console.
- Prefira mecanismos de rollback automático, que desfazem a mudança se você perder o acesso:
  - **Juniper Junos:** `commit confirmed 5`, depois `commit` definitivo.
  - **Cisco IOS-XR:** `commit confirmed 300`.
  - **Cisco IOS-XE:** `configure terminal revert timer 5` (com `archive` habilitado) e `configure confirm`.
  - **MikroTik RouterOS:** Safe Mode (`Ctrl+X`) antes da mudança.
  - **Huawei VRP, OLTs e Linux sem rollback nativo:** agende a reversão antes de aplicar (por exemplo `echo "cp /root/interfaces.pre-mudanca /etc/network/interfaces && ifreload -a" | at now + 5 minutes` no Linux) e cancele (`atrm`) só depois de confirmar o acesso.

## 4. Execute em passos pequenos
- Crie primeiro os objetos passivos (`ip-prefix`, `route-policy`, ACL, VLAN) e só no último passo vincule à interface ou ao peer.
- Valide a sintaxe antes de aplicar quando o sistema permitir (`nginx -t`, `ifreload -a -s`, `commit check`).
- Depois de cada passo de risco, confirme que a sessão ainda responde.

## 5. Pós-check e backup depois
- Compare com o estado do passo 1: prefixos, vizinhos, ONUs online, tráfego, CPU e logs.
- Se o critério de sucesso falhar, aplique o rollback imediatamente.
- Salve a configuração (`write memory`, `save`, `commit`), rode `net_backup` de novo e `net_diff` contra o backup anterior. Mostre o diff ao usuário como registro da mudança.
