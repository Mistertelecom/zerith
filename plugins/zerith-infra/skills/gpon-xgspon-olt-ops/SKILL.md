---
name: gpon-xgspon-olt-ops
description: Operação, provisionamento e troubleshooting de OLTs GPON/XGS-PON: Huawei (MA5800/MA5600T), ZTE (C300/C320/C600/C620), Datacom (DmOS), FiberHome e Nokia. Diagnóstico óptico (dBm, LOSi, Dying Gasp, Rogue ONU).
category: devops
---

# Operação & Troubleshooting de OLTs GPON / XGS-PON (Multi-Vendor)

Use esta skill sempre que trabalhar em **OLTs** e redes de acesso óptico **FTTH (GPON / XGS-PON)**.

## 1. Huawei SmartAX (`MA5600T` / `MA5800`)

- **Descoberta e status de ONTs:**
  ```text
  display ont autofind all
  display ont info summary 0/1/0
  display ont info 0 1 0 <ont-id>
  display ont optical-info 0 1 <pon> <ont-id>
  display ont register-info 0 1 <pon> <ont-id>
  display service-port port 0/1/<pon> ont <ont-id>
  display mac-address port 0/1/<pon> ont <ont-id>
  ```
- **Interpretação de causa de queda (`Last down cause`):**
  - `dying-gasp`: queda de energia elétrica na ONU do assinante.
  - `LOSi` / `LOBi`: rompimento de drop/fibra, conector sujo ou atenuação excessiva (`< -28 dBm`).
  - `LOFi` / `SF` / `SDi`: degradação óptica severa, reflexão ou **ONU Rogue** interferindo no time-slot TDMA da porta PON.

## 2. ZTE ZXA10 (`C300` / `C320` / `C600` / `C620`)

- **Comandos de diagnóstico em C300/C320:**
  ```text
  show gpon onu uncfg
  show gpon onu state gpon-olt_1/<slot>/<pon>
  show gpon onu detail-info gpon-onu_1/<slot>/<pon>:<onu-id>
  show pon power attenuation gpon-onu_1/<slot>/<pon>:<onu-id>
  show mac gpon onu gpon-onu_1/<slot>/<pon>:<onu-id>
  ```
- **Comandos de diagnóstico em C600/C620 (Titan):**
  ```text
  show pon onu uncfg
  show gpon onu state gpon_olt-1/<slot>/<pon>
  show pon power attenuation gpon_onu-1/<slot>/<pon>:<onu-id>
  ```

## 3. Datacom (`DmOS`) & FiberHome (`AN5516`)

- **Datacom DmOS:**
  - `show interface gpon <chassis>/<slot>/<port> onu`
  - `show interface gpon <chassis>/<slot>/<port> onu <id> optical-transceiver-diagnosis`
- **Faixas de Nível Óptico Saudável (GPON Classe B+ / C+):**
  - **Ideal:** `-15 dBm` a `-24 dBm`
  - **Alerta (Atenuação alta):** `-25 dBm` a `-27 dBm`
  - **Crítico (Queda/CRC/LOSi):** abaixo de `-27.5 dBm` ou acima de `-8 dBm` (saturação do fotodiodo).
