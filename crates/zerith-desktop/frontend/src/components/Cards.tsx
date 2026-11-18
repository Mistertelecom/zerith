import { useState } from "react";
import { Check, HelpCircle, ShieldAlert, ShieldCheck } from "lucide-react";
import type { Question } from "../lib/api";
import { toolIcon } from "./Transcript";

export function ApprovalCard(props: {
  tool: string;
  summary: string;
  onAnswer: (d: "once" | "session" | "deny") => void;
}) {
  const Icon = toolIcon(props.tool);
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
      <div className="ask-actions">
        <button className="btn btn-ghost" onClick={() => props.onAnswer("deny")}>
          Negar
        </button>
        <span className="spacer" />
        <button className="btn" onClick={() => props.onAnswer("once")}>
          Permitir uma vez
        </button>
        <button className="btn btn-primary" onClick={() => props.onAnswer("session")}>
          <ShieldCheck size={16} strokeWidth={1.75} />
          Permitir nesta conversa
        </button>
      </div>
    </div>
  );
}

export function QuestionCard(props: {
  questions: Question[];
  onAnswer: (answers: string[]) => void;
}) {
  const [picked, setPicked] = useState<Record<number, string[]>>({});
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
      <div className="ask-actions">
        <span className="spacer" />
        <button
          className="btn btn-primary"
          disabled={!complete}
          onClick={() => props.onAnswer(props.questions.map((_, i) => (picked[i] ?? []).join(", ")))}
        >
          Responder
        </button>
      </div>
    </div>
  );
}
