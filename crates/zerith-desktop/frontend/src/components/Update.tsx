import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { CircleAlert, CircleArrowUp, Download, RefreshCw } from "lucide-react";
import { updateApi, type UpdateInfo } from "../lib/api";
import { Row } from "./ui";

/** Last automatic check: `{ at, info }`. */
const KEY = "zerith.updateCheck";
/** A release shows up within this long while the app stays open. GitHub's
 * anonymous limit is 60 calls/hour, so this is far below it. */
const EVERY = 3 * 60 * 60 * 1000;
const DELAY = 15_000;

type Phase = "idle" | "checking" | "downloading" | "error";

/** Saves a check and tells the title bar pill, so a manual check in Ajustes
 * and the pill never disagree. */
function remember(info: UpdateInfo) {
  localStorage.setItem(KEY, JSON.stringify({ at: Date.now(), info }));
  window.dispatchEvent(new CustomEvent<UpdateInfo>("zerith:update", { detail: info }));
}

/** Download + SHA-256 + Ed25519 signature + installer; on success the app closes itself. */
function install(info: UpdateInfo, setPhase: (p: Phase) => void, setError: (e: string) => void) {
  if (!info.newer) return;
  setPhase("downloading");
  localStorage.removeItem(KEY); // the next start runs the new version: check fresh
  updateApi.install().catch((e) => {
    setError(String(e));
    setPhase("error");
  });
}

/** Title bar pill: checks 15 s after start, then every 3 h while open. */
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
    // offline is not worth a dialog: the next round tries again
    const check = () =>
      void updateApi
        .check()
        .then(remember)
        .catch(() => undefined);
    const t = setTimeout(check, DELAY);
    const every = setInterval(check, EVERY);
    return () => {
      clearTimeout(t);
      clearInterval(every);
    };
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

/** The running version, small, on the start screen; it also says when a
 * newer one is out (same event as the title bar pill). */
export function AppVersion() {
  const [version, setVersion] = useState("");
  const [latest, setLatest] = useState<string | null>(null);
  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => undefined);
    const seen = (e: Event) => {
      const i = (e as CustomEvent<UpdateInfo>).detail;
      setLatest(i.newer ? i.latest : null);
    };
    window.addEventListener("zerith:update", seen);
    return () => window.removeEventListener("zerith:update", seen);
  }, []);
  if (!version) return null;
  return (
    <p className="app-version">
      Zerith v{version}
      {latest ? <span className="app-version-new"> · nova versão {latest} disponível</span> : null}
    </p>
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
