// Custo em US$ (preços públicos do OpenRouter): a linha da conversa no
// painel de uso e o resumo por período na aba Uso. Ideia do CostTracker do
// Goose (aaif-goose/goose, Apache-2.0).
import { memo, useEffect, useState } from "react";
import { CircleDollarSign } from "lucide-react";
import { api, type CostLine, type CostView } from "../lib/api";
import { tr, type Lang } from "../lib/i18n";
import { Card, EmptyState, Segmented } from "./ui";

/** `US$ 0,0123`: quatro casas abaixo de um dólar, duas acima. */
export function usd(n: number, lang: Lang): string {
  const digits = n >= 1 ? 2 : 4;
  return `US$ ${n.toLocaleString(lang === "en" ? "en-US" : "pt-BR", {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  })}`;
}

/** Linhas de custo de um `CostView` para um `<dl className="insp-usage">`. */
export function CostRows({ cost, lang }: { cost: CostView; lang: Lang }) {
  const partial = cost.unpriced.length
    ? tr(lang, "Sem preço no catálogo, fora da soma: ", "Not in the price catalog, left out: ") +
      cost.unpriced.join(", ")
    : undefined;
  if (!cost.priced)
    return (
      <div title={partial}>
        <dt>{tr(lang, "Custo", "Cost")}</dt>
        <dd>{tr(lang, "desconhecido", "unknown")}</dd>
      </div>
    );
  const mark = partial ? " +?" : "";
  return (
    <>
      {cost.billed > 0 || cost.subscription === 0 ? (
        <div title={partial}>
          <dt>{tr(lang, "Custo", "Cost")}</dt>
          <dd>
            {usd(cost.billed, lang)}
            {mark}
          </dd>
        </div>
      ) : null}
      {cost.subscription > 0 ? (
        <div
          title={tr(
            lang,
            "Quanto este uso custaria na API, pelos preços públicos do OpenRouter. Na assinatura não é cobrado por token.",
            "What this usage would cost on the API, at OpenRouter's public prices. The subscription is not billed per token.",
          )}
        >
          <dt>{tr(lang, "Equivalente na API (assinatura)", "API equivalent (subscription)")}</dt>
          <dd>
            ~{usd(cost.subscription, lang)}
            {cost.billed > 0 ? "" : mark}
          </dd>
        </div>
      ) : null}
    </>
  );
}

const PERIODS = [
  ["24", "24 h"],
  ["168", "7 dias"],
  ["720", "30 dias"],
  ["0", "Tudo"],
] as const;

function lineText(l: CostLine): string {
  if (!l.priced) return "desconhecido";
  const parts: string[] = [];
  if (l.billed > 0 || l.subscription === 0) parts.push(usd(l.billed, "pt-BR"));
  if (l.subscription > 0) parts.push(`~${usd(l.subscription, "pt-BR")} assin.`);
  return parts.join(" + ");
}

/** Resumo de custo por período na aba Uso dos ajustes. */
export function CostCard() {
  const [hours, setHours] = useState("720");
  const [cost, setCost] = useState<CostView | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    setCost(null);
    setError(null);
    api
      .usageCost(null, Number(hours))
      .then((c) => live && setCost(c))
      .catch((e) => live && setError(String(e)));
    return () => {
      live = false;
    };
  }, [hours]);
  const total = cost ? cost.billed + cost.subscription : 0;
  return (
    <Card
      title="Custo"
      icon={CircleDollarSign}
      description="Preços públicos do OpenRouter, renovados a cada 24 h. Assinatura sai como equivalente na API: não é cobrada por token."
      actions={<Segmented value={hours} options={PERIODS} onPick={setHours} label="Período do custo" />}
    >
      {error ? (
        <p className="setting-help">{error}</p>
      ) : !cost ? (
        <div className="settings-loading">
          <span className="spinner" /> Calculando…
        </div>
      ) : cost.by_model.length === 0 ? (
        <EmptyState icon={CircleDollarSign} title="Sem uso neste período" />
      ) : (
        <>
          <dl className="insp-usage cost-total">
            <CostRows cost={cost} lang="pt-BR" />
          </dl>
          <div className="item-list usage-table cost-table">
            <div className="usage-head">
              <span>Modelo</span>
              <span>Participação</span>
              <span className="num-col">US$</span>
            </div>
            {cost.by_model.map((m) => (
              <div key={m.key} className="usage-row">
                <span className="item-title mono">{m.key}</span>
                <Share part={m.billed + m.subscription} whole={total} />
                <span className="num-col">{lineText(m)}</span>
              </div>
            ))}
            <div className="usage-head">
              <span>Dia</span>
              <span>Participação</span>
              <span className="num-col">US$</span>
            </div>
            {[...cost.by_day].reverse().map((d) => (
              <div key={d.key} className="usage-row">
                <span className="mono">{d.key}</span>
                <Share part={d.billed + d.subscription} whole={total} />
                <span className="num-col">{lineText(d)}</span>
              </div>
            ))}
          </div>
          {cost.unpriced.length ? (
            <p className="setting-help">
              Sem preço no catálogo, fora da soma: {cost.unpriced.join(", ")}
            </p>
          ) : null}
        </>
      )}
    </Card>
  );
}

function Share({ part, whole }: { part: number; whole: number }) {
  const pct = whole ? (part / whole) * 100 : 0;
  return (
    <span className="share">
      <span className="bar">
        <span style={{ width: pct + "%" }} />
      </span>
      <span className="pct">{Math.round(pct)}%</span>
    </span>
  );
}

/** Custo da conversa, relido quando o uso gravado muda (fim de um turno). */
export const SessionCost = memo(function SessionCost(p: { sessionId: string; requests: number; lang: Lang }) {
  const [cost, setCost] = useState<CostView | null>(null);
  useEffect(() => {
    let live = true;
    api
      .usageCost(p.sessionId)
      .then((c) => live && setCost(c))
      .catch(() => live && setCost(null));
    return () => {
      live = false;
    };
  }, [p.sessionId, p.requests]);
  if (!cost || cost.by_model.length === 0)
    return null;
  return <CostRows cost={cost} lang={p.lang} />;
});
