import { useEffect, useRef, useState } from "react";
import { Check, CircleAlert, HelpCircle, ShieldAlert, ShieldCheck } from "lucide-react";
import type { Question } from "../lib/api";
import { tr, useLang } from "../lib/i18n";
import { HighlightedCode, toolIcon } from "./Transcript";

export function ApprovalCard(props: {
  tool: string;
  summary: string;
  /** The allow rule "Sempre neste projeto" saves; absent: not offered. */
  always?: string | null;
  onAnswer: (d: "once" | "session" | "project" | "deny") => void | Promise<unknown>;
}) {
  const lang = useLang();
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

  const title = tr(lang, "Pedido de permissão", "Permission request");
  const codeLang = /^(net_|terminal_)/i.test(props.tool)
    ? "net"
    : /bash|shell|exec|command|powershell/i.test(props.tool)
      ? "sh"
      : "";

  return (
    <div className="ask" role="alertdialog" aria-label={title}>
      <div className="ask-head">
        <span className="ask-icon warn">
          <ShieldAlert size={16} strokeWidth={1.75} />
        </span>
        <div className="ask-title">
          <strong>{title}</strong>
          <p>
            {tr(
              lang,
              "O assistente quer usar uma ferramenta que precisa da sua aprovação.",
              "The assistant wants to use a tool that needs your approval.",
            )}
          </p>
        </div>
        <span className="badge mono">
          <Icon size={12} strokeWidth={2} />
          {props.tool}
        </span>
      </div>
      {/* O pedido mostra o texto exato (nada de markdown, que esconderia `*` ou
          um link); ganha realce conforme shell ou rede/terminal. */}
      <pre className="ask-body">
        <HighlightedCode lang={codeLang} text={props.summary} />
      </pre>
      {props.always ? (
        <p className="ask-rule">
          {lang === "pt-BR" ? (
            <>
              "Sempre neste projeto" grava a regra <code>{props.always}</code> nas suas permissões deste projeto.
            </>
          ) : (
            <>
              "Always in this project" saves rule <code>{props.always}</code> to your project permissions.
            </>
          )}
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
          {tr(lang, "Negar", "Deny")}
        </button>
        <span className="spacer" />
        {props.always ? (
          <button
            className="btn"
            disabled={busy}
            title={tr(
              lang,
              `Grava a regra ${props.always} para este projeto`,
              `Saves rule ${props.always} for this project`,
            )}
            onClick={() => void submit("project")}
          >
            {tr(lang, "Sempre neste projeto", "Always in this project")}
          </button>
        ) : null}
        <button className="btn" disabled={busy} onClick={() => void submit("session")}>
          {tr(lang, "Permitir nesta conversa", "Allow in this chat")}
        </button>
        <button className="btn btn-primary" disabled={busy} onClick={() => void submit("once")}>
          <ShieldCheck size={16} strokeWidth={1.75} />
          {tr(lang, "Permitir uma vez", "Allow once")}
        </button>
      </div>
    </div>
  );
}

export function QuestionCard(props: {
  questions: Question[];
  onAnswer: (answers: string[]) => void | Promise<unknown>;
}) {
  const lang = useLang();
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
    <div className="ask" role="group" aria-label={tr(lang, "Pergunta do assistente", "Assistant question")}>
      <div className="ask-head">
        <span className="ask-icon">
          <HelpCircle size={16} strokeWidth={1.75} />
        </span>
        <div className="ask-title">
          <strong>{tr(lang, "O assistente precisa de uma resposta", "The assistant needs an answer")}</strong>
          <p>
            {multi
              ? tr(lang, "Marque uma ou mais opções para ele continuar.", "Select one or more options to continue.")
              : tr(lang, "Escolha uma opção para ele continuar.", "Choose an option to continue.")}
          </p>
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
          {tr(lang, "Responder", "Submit")}
        </button>
      </div>
    </div>
  );
}
