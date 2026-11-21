---
name: wireguard-multi-client
description: Orquestração de VPNs WireGuard multi-cliente com faixas de IP sobrepostas (10.0.0.0/24, 192.168.1.0/24), cadastro 100% automático pela IA sem edição manual de inventory.toml, auto-descoberta de hosts e backups com diff.
category: devops
---

# Governança Multi-Cliente WireGuard & Auto-Cadastro Zero-Toque

Use esta skill sempre que o usuário mencionar um **cliente**, **VPN WireGuard**, **equipamentos com o mesmo IP (`10.0.0.1`) em clientes diferentes**, ou pedir para **cadastrar/descobrir equipamentos de um cliente**.

## 1. Cadastro 100% pela IA (O Usuário Nunca Edita `inventory.toml`)

1. **Salvar VPN WireGuard do Cliente (`save_vpn`):**
   - Quando o usuário colar o conteúdo de um arquivo `.conf` do WireGuard ou informar o caminho do arquivo:
     - Chame `net_inventory` com:
       ```json
       {
         "action": "save_vpn",
         "vpn": "wg-nomecliente",
         "client": "NomeCliente",
         "vpn_config": "[Interface]\nPrivateKey = ..."
       }
       ```
     - A ferramenta grava `~/.zerith/vpn/wg-nomecliente.conf` com permissão segura e já extrai `Address`, `AllowedIPs` e `Endpoint`.

2. **Cadastrar Equipamentos / VMs Linux / Proxmox com Cofre Automático (`upsert`):**
   - Chame `net_inventory` com `action: "upsert"` passando `client`, `name`, `host`, `user`, `password`, `vendor`, `role`, `site` e `vpn`:
     - A senha (`password`) é **gravada automaticamente no Cofre do Sistema** (`secret_ref = "net_<cliente>_<nome>"`) sem nunca ficar em texto puro no `inventory.toml`.
     - Se você passar `vpn_config` direto no `upsert`, o `.conf` da VPN e o equipamento são salvos em uma única chamada.

3. **Auto-Descoberta da Rede do Cliente após ligar a VPN (`net_probe` com `auto_register_client`):**
   - Se o cliente só forneceu a VPN e você quer descobrir e cadastrar todos os equipamentos da faixa do `AllowedIPs` (ex.: `10.0.0.1-30` ou `10.0.0.0/28`):
     1. Ligue a VPN com `net_vpn(action="switch", vpn="wg-nomecliente")`.
     2. Rode `net_probe(subnet="10.0.0.1-30", auto_register_client="NomeCliente", vpn="wg-nomecliente")`.
     3. O `net_probe` identifica automaticamente servidores **Proxmox VE (`8006`)**, **MikroTik (`8291`/`8728`/`ROSSSH`)**, **Huawei VRP**, **Cisco IOS**, **FortiGate (`8443`)** e **VMs Linux (`22`)** e já os cadastra no inventário do cliente!

## 2. Chaveamento Exclusivo de VPN WireGuard (`net_vpn` & Auto-Switch no `net_connect`)

- Clientes frequentemente usam a mesma sub-rede (`10.0.0.0/24` ou `192.168.1.0/24`). **Nunca mantenha duas VPNs de clientes com IPs sobrepostos ligadas ao mesmo tempo.**
- Ao chamar `net_connect(device="core-01", client="NomeCliente")`:
  - O Zerith verifica automaticamente o campo `vpn` do dispositivo e executa o switch exclusivo (desliga a VPN do cliente anterior, encerra terminais órfãos daquele túnel e liga a VPN do cliente atual antes de abrir o SSH/Telnet).
- Quando o usuário disser *"terminei o atendimento do cliente X"*, feche os terminais e chame `net_vpn(action="down")`.

## 3. Operações em Lote Paralelas (`net_broadcast`) e Snapshots (`net_backup`)

- Para rodar o mesmo diagnóstico em vários roteadores ou VMs Linux de um cliente simultaneamente, use **`net_broadcast(command="...", client="NomeCliente")`**.
- Antes e depois de qualquer janela de manutenção, rode **`net_backup(client="NomeCliente")`** para salvar a configuração em `backups/<cliente>/<equipamento>/latest.cfg` e gerar automaticamente o `net_diff` + script de rollback contra o backup anterior.
