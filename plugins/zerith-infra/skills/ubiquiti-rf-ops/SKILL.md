---
name: ubiquiti-rf-ops
description: "Use em enlaces sem fio e equipamentos Ubiquiti (airOS M/AC, LTU, airFiber, UISP e UniFi): coleta via SSH/BusyBox (mca-status, wstalist), UISP REST API, UniFi API, SNMP (net_snmp preset ubiquiti_rf), engenharia RF (Chain0/1 balance, CINR/SNR, Noise Floor, Airtime, DFS, Fresnel/FSPL) e rollback em Test Mode."
---

# Ubiquiti RF Ops (airOS / LTU / airFiber / UISP / UniFi)

Em enlaces PtP e PtMP de provedor, sinal RSSI sozinho engana: um enlace com `-55 dBm` pode estar ruim se tiver desbalanceamento de polarização, CINR baixo ou zona de Fresnel obstruída.

## 1. Coleta multi-canal com Zerith
- **SNMP nativo:** `net_snmp(device=..., preset="ubiquiti_rf")` coleta frequência, TX power, signal, noise floor, CCQ, airMAX Quality/Capacity e stations.
- **SSH / BusyBox (`net_connect` + `terminal_batch`):**
  - airOS / airMAX AC: `mca-status`, `wstalist`, `cat /tmp/system.cfg`, `athstats`
  - UniFi AP: `mca-cli-op info`, `iwconfig`
- **APIs (`net_api`):**
  - **UISP:** `GET /nms/api/v2.1/devices?withInterfaces=true`
  - **UniFi:** endpoints `/api/s/<site>/stat/sta` e `/stat/device`
- **Auditoria e matemática RF:**
  - Comandos: `net_audit(action="ubiquiti_rf_audit", model="airmax_ac", mode="commands")`
  - Análise + FSPL: `net_audit(action="ubiquiti_rf_audit", mode="parse", output=..., antenna_gain_dbi=..., freq_mhz=..., distance_km=...)`

## 2. Engenharia RF e limites saudáveis
- **Balanceamento Chain0 / Chain1 (`|chain0 - chain1|`):**
  - `< 3 dB`: alinhamento excelente.
  - `>= 5 dB` (alerta) / `>= 8 dB` (crítico): indica antena desalinhada, obstrução parcial de Fresnel ou problema físico em pigtail/conector.
- **SNR e CINR:**
  - `SNR = RSSI - Noise Floor` (ideal `>= 25 dB`; `< 20 dB` alerta; `< 15 dB` crítico).
  - **CINR** reflete relação sinal sobre interferência+ruído: se SNR está alto mas CINR está baixo (`< 20 dB`), há interferência co-canal/adjacente.
- **CCQ e Airtime:**
  - `CCQ < 85%` aponta retransmissões; em PtMP, um único cliente com sinal ruim consome **Airtime** excessivo e derruba todo o painel (`Airtime > 75%` = saturação).
- **FSPL (Free-Space Path Loss) e Fresnel:**
  - Fórmula: `FSPL (dB) = 32.44 + 20*log10(d_km) + 20*log10(f_MHz)`.
  - `RSSI esperado = TX_Power + Ganho_TX + Ganho_RX - FSPL`.
  - Se `RSSI esperado - RSSI medido >= 5 a 10 dB`, há obstrução na 1ª zona de Fresnel ou perda em cabo/alinhamento.

## 3. DFS e Mudança Segura (Test Mode)
- Canais **DFS (5250–5725 MHz)** sofrem radar detection e derrubam o enlace por 1–10 min em CAC; mantenha frequência backup documentada.
- **Ordem obrigatória em PtP:** altere frequência/largura de canal/SSID **primeiro na ponta remota (Station)** e depois no **AP**, usando **Test Mode** (revert automático em ~180s caso o enlace não feche).
- Gere a receita passo a passo: `net_audit(action="ubiquiti_rf_audit", mode="plan_change")`.
