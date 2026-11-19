import { useCallback, useEffect, useState } from "react";
import { Archive, CircleAlert, CircleArrowUp, Database, FolderOpen, Gauge, Leaf, Palette, ShieldCheck } from "lucide-react";
import { api, learnApi, type ProfileView, type StorageView } from "../lib/api";
import { Card, Row, Segmented, Switch } from "./ui";
import { UpdateRow } from "./Update";

const THEMES = [
  ["dark", "Escuro"],
  ["light", "Claro"],
  ["system", "Sistema"],
] as const;

const PROFILES = [
  ["economy", "Economia"],
  ["balanced", "Balanceado"],
  ["performance", "Desempenho"],
] as const;

const PONYTAIL = [
  ["off", "Desligado"],
  ["lite", "Leve"],
  ["full", "Médio"],
  ["ultra", "Máximo"],
] as const;

const CAVEMAN = [
  ["off", "Desligado"],
  ["lite", "Leve"],
  ["full", "Médio"],
  ["ultra", "Máximo"],
  ["wenyan", "Wenyan"],
] as const;

export interface GeneralProps {
  theme: string;
  reduceMotion: boolean;
  profile: string;
  memoryPaused: boolean;
  /** The composer's model list (`conta:modelo`), for the learning reviewer. */
  models: { value: string; label: string }[];
  onTheme: (t: string) => void;
  onReduceMotion: (v: boolean) => void;
  onProfile: (p: string) => void;
  onMemory: (paused: boolean) => void;
}

const human = (bytes: number) =>
  bytes >= 1 << 30
    ? `${(bytes / (1 << 30)).toFixed(1)} GB`
    : bytes >= 1 << 20
      ? `${(bytes / (1 << 20)).toFixed(1)} MB`
      : bytes >= 1 << 10
        ? `${Math.round(bytes / 1024)} KB`
        : `${bytes} B`;

export function GeneralTab(props: GeneralProps) {
  const [limits, setLimits] = useState<ProfileView[]>([]);
  const [storage, setStorage] = useState<StorageView | null>(null);
  const [backup, setBackup] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [learning, setLearning] = useState<boolean | null>(null);
  const [learnModel, setLearnModel] = useState("");

  useEffect(() => {
    learnApi
      .get()
      .then((l) => {
        setLearning(l.enabled);
        setLearnModel(l.model);
      })
      .catch(() => undefined);
  }, []);
  const toggleLearning = (v: boolean) => {
    setLearning(v);
    learnApi
      .set({ enabled: v })
      .then((l) => setLearning(l.enabled))
      .catch((e) => setError(String(e)));
  };
  const pickLearnModel = (m: string) => {
    setLearnModel(m);
    learnApi
      .set({ model: m })
      .then((l) => setLearnModel(l.model))
      .catch((e) => setError(String(e)));
  };

  useEffect(() => {
    api.profileLimits().then(setLimits).catch(() => undefined);
    api
      .storage()
      .then(setStorage)
      .catch((e) => setError(String(e)));
  }, []);

  const limit = limits.find((l) => l.name === props.profile);

  const copy = useCallback(() => {
    setBusy(true);
    api
      .backup()
      .then((p) => {
        setBackup(p);
        setError(null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setBusy(false));
  }, []);

  return (
    <div className="page pn">
      {error ? (
        <div className="toast bad" role="alert">
          <CircleAlert size={16} strokeWidth={1.75} />
          <span>{error}</span>
        </div>
      ) : null}
      <Card title="Aparência" icon={Palette} description="Como a janela se apresenta.">
        <Row
          label="Tema"
          hint="“Sistema” acompanha o modo claro ou escuro do computador."
        >
          <Segmented
            label="Tema"
            value={props.theme}
            options={THEMES}
            onPick={props.onTheme}
          />
        </Row>
        <Row
          label="Movimento reduzido"
          hint="Troca animações por cortes e fades curtos."
        >
          <Switch label="Movimento reduzido" checked={props.reduceMotion} onChange={props.onReduceMotion} />
        </Row>
      </Card>

      <Card title="Sobre o Zerith" icon={CircleArrowUp} description="Novas versões chegam direto do GitHub.">
        <UpdateRow />
      </Card>

      <Card title="Privacidade" icon={ShieldCheck} description="O que o agente pode lembrar entre conversas.">
        <Row
          label="Pausar memória"
          hint="Enquanto estiver ligado, as próximas conversas não leem nem gravam lembranças."
        >
          <Switch label="Pausar memória" checked={props.memoryPaused} onChange={props.onMemory} />
        </Row>
        <Row
          label="Aprender sozinho (memória e skills)"
          tech="revisão em segundo plano"
          hint="Depois de um turno com correção, preferência ou trabalho com várias ferramentas, uma revisão rápida guarda o que vale lembrar. Turnos simples não custam nada. Cada item aparece no chat com Desfazer."
        >
          <Switch
            label="Aprender sozinho"
            checked={learning ?? true}
            disabled={learning === null}
            onChange={toggleLearning}
          />
        </Row>
        <Row
          label="Modelo da revisão"
          hint="Um modelo barato basta: a revisão lê só um resumo do turno. “Mesmo da conversa” usa o modelo que respondeu."
        >
          <select
            className="select"
            aria-label="Modelo da revisão"
            title={learnModel || "Mesmo da conversa"}
            value={learnModel}
            disabled={learning === null || learning === false}
            onChange={(e) => pickLearnModel(e.target.value)}
          >
            <option value="">Mesmo da conversa</option>
            {/* a saved model that is no longer offered still shows, so the
                select never lies about what is stored */}
            {learnModel && !props.models.some((m) => m.value === learnModel) ? (
              <option value={learnModel}>{learnModel}</option>
            ) : null}
            {props.models.map((m) => (
              <option key={m.value} value={m.value}>
                {m.label}
              </option>
            ))}
          </select>
        </Row>
      </Card>

      <Card title="Perfil de execução" icon={Gauge} description="Troca velocidade por custo.">
        <Row
          label="Perfil"
          hint={
            limit
              ? `Quanto o agente lê e delega por turno: ${limit.historyWindow} mensagens de histórico e até ${limit.maxSubagents} subagentes em paralelo.`
              : "Quanto o agente lê e delega por turno."
          }
        >
          <Segmented
            label="Perfil de execução"
            value={props.profile}
            options={PROFILES}
            onPick={props.onProfile}
          />
        </Row>
      </Card>

      <Card title="Seus dados" icon={Database} description="Tudo fica local, no seu computador.">
        <Row
          label="Pasta de dados"
          hint="Conversas, memória, skills e configurações ficam aqui, só no seu computador."
        >
          <span className="mono-path" title={storage?.root}>
            {storage?.root ?? "…"}
          </span>
          <button
            className="btn btn-sm"
            onClick={() => void api.revealRoot().catch((e) => setError(String(e)))}
          >
            <FolderOpen size={14} strokeWidth={2} /> Abrir
          </button>
        </Row>
        <Row
          label="Banco de conversas"
          hint={
            storage
              ? `${human(storage.dbBytes)} · ${storage.sessions} conversas · ${storage.messages} mensagens`
              : "Calculando…"
          }
        >
          <span className="mono-path" title={storage?.dbPath}>
            {storage?.dbPath ?? "…"}
          </span>
        </Row>
        <Row
          label="Backup"
          hint={
            backup
              ? `Cópia salva em ${backup}`
              : "Cópia consistente do banco na pasta backups/, feita com o app aberto."
          }
        >
          <button className="btn btn-sm" disabled={busy} onClick={() => void copy()}>
            {busy ? <span className="spinner" /> : <Archive size={14} strokeWidth={2} />} {busy ? "Copiando…" : "Fazer backup"}
          </button>
        </Row>
      </Card>

      <Card title="Economia de tokens" icon={Leaf} description="Menos tokens por turno, mesma resposta.">
        <SaverRows />
      </Card>
    </div>
  );
}

/// The three ways to spend fewer tokens. Each one saves on change and reads
/// back what the file accepted, so the control never lies.
function SaverRows() {
  const [saver, setSaver] = useState<{ ponytail: string; caveman: string; rtkEnabled: boolean } | null>(
    null,
  );
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.tokenSaver().then(setSaver).catch((e) => setError(String(e)));
  }, []);

  const patch = (p: { ponytail?: string; caveman?: string; rtkEnabled?: boolean }) => {
    api
      .setTokenSaver(p)
      .then((s) => {
        setSaver(s);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  };

  if (!saver)
    return error ? (
      <div className="pn-callout danger">
        <CircleAlert size={14} strokeWidth={2} />
        <span>{error}</span>
      </div>
    ) : (
      <div className="pn-loading">
        <span className="spinner" /> Lendo…
      </div>
    );
  return (
    <>
      {error ? (
        <div className="pn-callout danger">
          <CircleAlert size={14} strokeWidth={2} />
          <span>{error}</span>
        </div>
      ) : null}
      <Row
        label="Código enxuto"
        tech="Ponytail"
        hint="Pede ao agente o mínimo que resolve: reaproveitar o que já existe, sem camadas a mais. Nível mais alto, diff menor."
      >
        <Segmented
          label="Código enxuto"
          value={saver.ponytail}
          options={PONYTAIL}
          onPick={(v) => patch({ ponytail: v })}
        />
      </Row>
      <Row
        label="Respostas curtas"
        tech="Caveman"
        hint="O agente explica em poucas palavras. Código, caminhos, comandos e avisos de segurança continuam completos."
      >
        <Segmented
          label="Respostas curtas"
          value={saver.caveman}
          options={CAVEMAN}
          onPick={(v) => patch({ caveman: v })}
        />
      </Row>
      <Row
        label="Resumir saídas longas"
        tech="RTK"
        hint="Comandos como git, build e grep podem imprimir milhares de linhas. Só o essencial vai ao modelo; o texto completo continua no terminal."
      >
        <Switch label="Resumir saídas longas" checked={saver.rtkEnabled} onChange={(v) => patch({ rtkEnabled: v })} />
      </Row>
    </>
  );
}