import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { CircleAlert, CircleArrowUp, Download, RefreshCw } from "lucide-react";
import { updateApi, type UpdateInfo } from "../lib/api";
import { Row } from "./ui";

/** Last automatic check: `{ at, info }`. One network check per day at most. */
const KEY = "zerith.updateCheck";
const DAY = 24 * 60 * 60 * 1000;
const DELAY = 15_000;

type Phase = "idle" | "checking" | "downloading" | "error";

/** Saves a check and tells the title bar pill, so a manual check in Ajustes
 * and the pill never disagree. */
function remember(info: UpdateInfo) {
  localStorage.setItem(KEY, JSON.stringify({ at: Date.now(), info }));
  window.dispatchEvent(new CustomEvent<UpdateInfo>("zerith:update", { detail: info }));
}

/** Download + SHA-256 + installer; on success the app closes itself. */
function install(info: UpdateInfo, setPhase: (p: Phase) => void, setError: (e: string) => void) {
  if (!info.url) return;
  setPhase("downloading");
  localStorage.removeItem(KEY); // the next start runs the new version: check fresh
  updateApi.install(info.url).catch((e) => {
    setError(String(e));
    setPhase("error");
  });
}

/** Title bar pill: 15 s after start, at most one GitHub call per 24 h. */
export function UpdatePill() {
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const [phase, setPhase] = useState<Phase>("idle");
  const [error, setError] = useState("");

  useEffect(() => {
    const seen = (e: Event) => {
      const i = (e as CustomEvent<UpdateInfo>).detail;
      setInfo(i.newer ? i : null);
    };
    window.addEventListener("zerith:update", seen);
    return () => window.removeEventListener("zerith:update", seen);
  }, []);

  useEffect(() => {
    const t = setTimeout(() => {
      let saved: { at: number; info: UpdateInfo | null } | null = null;
      try {
        saved = JSON.parse(localStorage.getItem(KEY) ?? "null");
      } catch {
        /* bad entry: check again */
      }
      if (saved && Date.now() - saved.at < DAY) {
        const cached = saved.info;
        // still newer than what runs now (the user may have updated by hand)
        if (cached?.newer)
          void getVersion()
            .then((v) => cached.latest !== v && setInfo(cached))
            .catch(() => undefined);
        return;
      }
      updateApi
        .check()
        .then(remember)
        .catch(() => undefined); // offline at start is not worth a dialog
    }, DELAY);
    return () => clearTimeout(t);
  }, []);

  if (!info) return null;
  return (
    <div className={"update-pill" + (phase === "error" ? " bad" : "")} role="status">
      {phase === "error" ? (
        <CircleAlert size={14} strokeWidth={2} />
      ) : phase === "downloading" ? (
        <span className="spinner" />
      ) : (
        <CircleArrowUp size={14} strokeWidth={2} />
      )}
      <span className="update-text" title={phase === "error" ? error : info.notes || undefined}>
        {phase === "downloading"
          ? "Baixando…"
          : phase === "error"
            ? `Falhou: ${error}`
            : `Nova versão ${info.latest} disponível`}
      </span>
      <button
        className="update-go"
        disabled={phase === "downloading"}
        onClick={() => install(info, setPhase, setError)}
      >
        {phase === "error" ? "Tentar de novo" : "Atualizar"}
      </button>
      {phase !== "downloading" ? (
        <button className="update-later" onClick={() => setInfo(null)}>
          Depois
        </button>
      ) : null}
    </div>
  );
}

/** Ajustes › Geral: the running version and a manual check. */
export function UpdateRow() {
  const [version, setVersion] = useState("…");
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const [phase, setPhase] = useState<Phase>("idle");
  const [error, setError] = useState("");

  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => undefined);
  }, []);

  const check = () => {
    setPhase("checking");
    updateApi
      .check()
      .then((i) => {
        setInfo(i);
        setVersion(i.current);
        setPhase("idle");
        remember(i);
      })
      .catch((e) => {
        setError(String(e));
        setPhase("error");
      });
  };

  return (
    <Row
      label="Atualizações"
      hint={
        <>
          Versão atual {version}
          {phase === "error" ? (
            <span className="update-result bad"> · {error}</span>
          ) : phase === "downloading" ? (
            <span className="update-result"> · Baixando o instalador…</span>
          ) : info ? (
            <span className="update-result"> · {info.message}</span>
          ) : null}
        </>
      }
    >
      {info?.newer ? (
        <button
          className="btn btn-sm btn-primary"
          disabled={phase === "downloading"}
          onClick={() => install(info, setPhase, setError)}
        >
          {phase === "downloading" ? <span className="spinner" /> : <Download size={14} strokeWidth={2} />}{" "}
          {phase === "downloading" ? "Baixando…" : `Atualizar para ${info.latest}`}
        </button>
      ) : null}
      <button className="btn btn-sm" disabled={phase === "checking" || phase === "downloading"} onClick={check}>
        {phase === "checking" ? <span className="spinner" /> : <RefreshCw size={14} strokeWidth={2} />}{" "}
        {phase === "checking" ? "Verificando…" : "Verificar agora"}
      </button>
    </Row>
  );
}
