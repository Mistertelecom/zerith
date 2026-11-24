import { useEffect, useRef, useState } from "react";
import { Check, CircleAlert, HelpCircle, ShieldAlert, ShieldCheck } from "lucide-react";
import type { Question } from "../lib/api";
import { toolIcon } from "./Transcript";

export function ApprovalCard(props: {
  tool: string;
  summary: string;
  /** The allow rule "Sempre neste projeto" saves; absent: not offered. */
  always?: string | null;
  onAnswer: (d: "once" | "session" | "project" | "deny") => void | Promise<unknown>;
}) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const Icon = toolIcon(props.tool);
  // Focus "Negar" once, when the card appears (keyed by approval id in App).
  const deny = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    deny.current?.focus();
  }, []);

  const submit = async (d: "once" | "session" | "project" | "deny") => {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      await props.onAnswer(d);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="ask" role="alertdialog" aria-label="Pedido de permissão">
      <div className="ask-head">
        <span className="ask-icon warn">
          <ShieldAlert size={16} strokeWidth={1.75} />
        </span>
        <div className="ask-title">
          <strong>Pedido de permissão</strong>
          <p>O assistente quer usar uma ferramenta que precisa da sua aprovação.</p>
        </div>
        <span className="badge mono">
          <Icon size={12} strokeWidth={2} />
          {props.tool}
        </span>
      </div>
      <pre className="ask-body">{props.summary}</pre>
      {props.always ? (
        <p className="ask-rule">
          "Sempre neste projeto" grava a regra <code>{props.always}</code> nas suas permissões deste projeto.
        </p>
      ) : null}
      {err ? (
        <div className="toast bad" role="alert">
          <CircleAlert size={15} strokeWidth={1.75} />
          <span>{err}</span>
        </div>
      ) : null}
      <div className="ask-actions">
        <button ref={deny} className="btn btn-ghost" disabled={busy} onClick={() => void submit("deny")}>
          Negar
        </button>
        <span className="spacer" />
        {props.always ? (
          <button
            className="btn"
            disabled={busy}
            title={`Grava a regra ${props.always} para este projeto`}
            onClick={() => void submit("project")}
          >
            Sempre neste projeto
          </button>
        ) : null}
        <button className="btn" disabled={busy} onClick={() => void submit("session")}>
          Permitir nesta conversa
        </button>
        <button className="btn btn-primary" disabled={busy} onClick={() => void submit("once")}>
          <ShieldCheck size={16} strokeWidth={1.75} />
          Permitir uma vez
        </button>
      </div>
    </div>
  );
}

export function QuestionCard(props: {
  questions: Question[];
  onAnswer: (answers: string[]) => void | Promise<unknown>;
}) {
  const [picked, setPicked] = useState<Record<number, string[]>>({});
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const pick = (qi: number, label: string) => {
    const q = props.questions[qi];
    setPicked((prev) => {
      const cur = prev[qi] ?? [];
      if (q.multi_select) {
        return {
          ...prev,
          [qi]: cur.includes(label) ? cur.filter((l) => l !== label) : [...cur, label],
        };
      }
      return { ...prev, [qi]: [label] };
    });
  };
  const complete = props.questions.every((_, i) => (picked[i] ?? []).length > 0);
  const multi = props.questions.some((q) => q.multi_select);

  const submit = async () => {
    if (!complete || busy) return;
    setBusy(true);
    setErr(null);
    try {
      await props.onAnswer(props.questions.map((_, i) => (picked[i] ?? []).join(", ")));
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="ask" role="group" aria-label="Pergunta do assistente">
      <div className="ask-head">
        <span className="ask-icon">
          <HelpCircle size={16} strokeWidth={1.75} />
        </span>
        <div className="ask-title">
          <strong>O assistente precisa de uma resposta</strong>
          <p>{multi ? "Marque uma ou mais opções para ele continuar." : "Escolha uma opção para ele continuar."}</p>
        </div>
      </div>
      {props.questions.map((q, qi) => (
        <div className="question" key={qi}>
          <div className="question-head">
            <span className="badge">{q.header}</span>
            <span>{q.question}</span>
          </div>
          <div className="question-options" role={q.multi_select ? "group" : "radiogroup"}>
            {q.options.map((o) => {
              const on = (picked[qi] ?? []).includes(o.label);
              return (
                <button
                  key={o.label}
                  role={q.multi_select ? "checkbox" : "radio"}
                  aria-checked={on}
                  disabled={busy}
                  className={"option" + (on ? " option-on" : "")}
                  onClick={() => pick(qi, o.label)}
                >
                  <span className={"option-mark" + (q.multi_select ? " multi" : "")}>
                    {on ? <Check size={11} strokeWidth={3} /> : null}
                  </span>
                  <span className="option-text">
                    <strong>{o.label}</strong>
                    {o.description ? <span>{o.description}</span> : null}
                  </span>
                </button>
              );
            })}
          </div>
        </div>
      ))}
      {err ? (
        <div className="toast bad" role="alert">
          <CircleAlert size={15} strokeWidth={1.75} />
          <span>{err}</span>
        </div>
      ) : null}
      <div className="ask-actions">
        <span className="spacer" />
        <button
          className="btn btn-primary"
          disabled={!complete || busy}
          onClick={() => void submit()}
        >
          Responder
        </button>
      </div>
    </div>
  );
}
