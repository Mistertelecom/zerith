---
name: value-investing-scanner
description: "Use quando o usuário pedir ações ou FIIs recomendados para comprar hoje, ações baratas ou abaixo do mercado/preço justo (B3 ou bolsas dos EUA/globais), análise fundamentalista, múltiplos P/L, P/VP, EV/EBITDA, Dividend Yield, preço justo de Graham/Bazin ou criação de carteira e alertas financeiros."
---

# Analista Fundamentalista & Scanner de Ações Descontadas (B3 & Global)

Sempre que o usuário perguntar quais ações ou ativos estão **abaixo do mercado / baratos / descontados** ou pedir análise para compra, siga este protocolo objetivo com dados reais:

## 1. Nunca chute cotações ou múltiplos — colete dados ao vivo primeiro

1. **Coleta zero-API-key via `web_search` / `web_fetch` ou `shell` (`curl` / Python `urllib`):**
   - **Ações dos EUA / Globais (Yahoo Finance público):**
     - `https://query1.finance.yahoo.com/v8/finance/chart/<TICKER>?range=1y&interval=1d`
     - Buscar fundamentos recentes via `web_search` (`"<TICKER>" PE PB ROE free cash flow valuation`) quando endpoints de resumo exigirem crumb.
   - **Ações da B3 (`*.SA`, FIIs, BDRs):**
     - Brapi público (`https://brapi.dev/api/quote/<TICKER>` ou consulta `web_fetch`/`web_search` em fontes abertas como Fundamentus / StatusInvest / Yahoo `<TICKER>.SA`).
   - **Catalisadores dos últimos 30 dias:** cruzar notícias recentes (balanços trimestrais, fatos relevantes, mudanças de guidance, recompras, dividendos ou sentimento em fóruns/notícias).
2. Se alguma ferramenta ou endpoint de rede estiver bloqueado ou sem internet, informe explicitamente quais dados faltaram — **jamais invente preço ou indicador**.

## 2. Metodologia de Valuation e Margem de Segurança

Para cada ativo candidato, calcule quando os dados (`LPA`, `VPA`, `Dividendo Anual`, `Cotação Atual`) estiverem disponíveis:
- **Preço Justo de Graham:** `sqrt(22.5 * LPA * VPA)` (aplicável quando LPA > 0 e VPA > 0).
- **Preço Teto de Bazin:** `Provento Anual por Ação / 0.06` (para estratégias focadas em dividendos >= 6% ao ano).
- **Margem de Segurança (%):** `((Preço Justo - Preço Atual) / Preço Justo) * 100`.
- **Qualidade & Armadilha de Valor (*Value Trap Check*):** verifique também `ROE`, `Dívida Líquida / EBITDA`, consistência de lucro e se o desconto se deve a queda pontual ou deterioração estrutural.

## 3. Formato de Resposta Obrigatório

Apresente sempre:
1. **Tabela Comparativa:**
   `| Ticker | Setor | Preço Atual | Preço Justo (Graham/Bazin) | Desconto % | P/L | P/VP | DY (12m) | Risco Principal |`
2. **Síntese Executiva:** top candidatos por margem de segurança + por que o mercado está precificando com desconto + o que pode destravar valor (tese *bull* vs *bear*).
3. **Automação Pessoal (Opcional):** lembre ou ofereça configurar um workflow no **Zerith Workflows** (ex.: todo dia útil às `09:30` antes ou após a abertura do pregão) enviando esse radar no WhatsApp/Telegram usando `workflow_create_or_update`.
4. **Aviso transparente:** inclua 1 linha informando que a análise tem caráter informativo/educacional baseado em dados públicos e não substitui recomendação personalizada certificada (CVM/SEC).
