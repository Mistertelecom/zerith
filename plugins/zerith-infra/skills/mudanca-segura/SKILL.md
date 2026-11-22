---
name: mudanca-segura
description: "Use ao planejar ou executar janelas de manutenção (GMUD/MOP) em ativos de rede de produção: pré-check, comandos de alteração, plano de rollback imediato e pós-check."
---

# Janela de Manutenção Segura (MOP / GMUD de Redes)

Nenhuma alteração em equipamento de produção deve ser aplicada sem um **pré-check**, **mecanismo contra isolamento** e **bloco de rollback explícito**.

## 1. Fluxo Obrigatório em 4 Etapas

1. **Pré-Check (Baseline antes da mudança):**
   - Coletar e salvar estado atual das sessões BGP/OSPF/BFD, tabela de rotas para os prefixos afetados, tráfego nas interfaces de uplink e logs recentes (`display logbuffer` / `show logging`).
2. **Proteção contra Perda de Gerência:**
   - **Juniper Junos:** usar sempre `commit confirmed 5` antes do `commit` definitivo.
   - **Cisco IOS-XR:** usar `commit confirmed 300`.
   - **Cisco IOS-XE:** usar `configure terminal revert timer 5` (com `archive` habilitado).
   - **Huawei VRP:** validar a gerência in-band/out-of-band e preparar o bloco `undo` completo antes de colar alterações em ACL, política de rota ou interface de uplink.
   - **MikroTik RouterOS:** ativar **Safe Mode** (`Ctrl+X`) antes de alterar firewall, VLAN de gerência ou sessão BGP de upstream.
3. **Execução Incremental:**
   - Criar primeiro os objetos passivos (`ip-prefix`, `route-policy`, `acl`, `vlan`) e só no último passo vinculá-los à interface ou ao peer BGP.
4. **Pós-Check e Critério de Rollback:**
   - Comparar contagem de prefixos recebidos/anunciados, latência, perda de pacotes e CPU do plano de controle. Se o critério de sucesso não for atingido no tempo estipulado, executar imediatamente o bloco de **Rollback**.
