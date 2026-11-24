import { useEffect, useMemo, useState } from "react";
import {
  Check,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  ExternalLink,
  Film,
  FolderOpen,
  Image as ImageIcon,
  ImageOff,
  KeyRound,
  RefreshCw,
  Save,
  Search,
  WandSparkles,
} from "lucide-react";
import {
  api,
  type MediaFile,
  type MediaKey,
  type MediaModel,
  type MediaSlot,
  type ParamView,
} from "../lib/api";
import { Card, EmptyState, Logo, Row, Segmented, Switch } from "./ui";

const KINDS = [
  ["image", "Imagem"],
  ["video", "Vídeo"],
] as const;

const SOURCE: Record<string, string> = {
  keyring: "Cofre do sistema",
  env: "Variável de ambiente",
};

const money = (v: number | null) =>
  v === null ? "—" : v.toLocaleString("pt-BR", { style: "currency", currency: "USD" });

/** "Google: Veo 3.1" -> ["Google", "Veo 3.1"]; no prefix: the id's vendor. */
function split(m: MediaModel): { vendor: string; short: string } {
  const i = m.name.indexOf(": ");
  if (i > 0) return { vendor: m.name.slice(0, i), short: m.name.slice(i + 2) };
  return { vendor: m.id.split("/")[0], short: m.name };
}

const vendorKey = (m: MediaModel) => m.id.split("/")[0];

/** Short facts for the card: durations, resolutions, audio, references. */
function capabilities(m: MediaModel): string[] {
  const out: string[] = [];
  const opt = (name: string) => m.params.find((p) => p.name === name);
  const dur = opt("duration");
  if (dur?.kind === "enum" && dur.options.length) {
    const n = dur.options.map(Number).filter((x) => Number.isFinite(x));
    out.push(
      n.length > 3
        ? `${Math.min(...n)}–${Math.max(...n)} s`
        : `${dur.options.join(", ")} s`,
    );
  }
  const res = opt("resolution");
  if (res?.kind === "enum" && res.options.length) out.push(res.options.join(" · "));
  if (opt("generate_audio")) out.push("Áudio");
  if (m.maxReferences > 0)
    out.push(m.maxReferences === 1 ? "1 referência" : `até ${m.maxReferences} referências`);
  if (m.frameImages.length)
    out.push(m.frameImages.length > 1 ? "1º e último quadro" : "Quadro inicial");
  const ar = opt("aspect_ratio");
  if (ar?.kind === "enum" && ar.options.length > 1) out.push(`${ar.options.length} proporções`);
  return out;
}

/// One optional default per parameter. Empty = the chat model (or the model)
/// decides.
function ParamField(props: {
  spec: ParamView;
  value: unknown;
  onChange: (v: unknown) => void;
}) {
  const { spec, value, onChange } = props;
  if (spec.kind === "flag") {
    return (
      <div className="field">
        <span className="field-label mono">{spec.name}</span>
        <Switch label={spec.name} checked={value === true} onChange={(v) => onChange(v || undefined)} />
      </div>
    );
  }
  if (spec.kind === "enum") {
    return (
      <label className="field">
        <span className="field-label mono">{spec.name}</span>
        <select
          className="select"
          value={String(value ?? "")}
          onChange={(e) => onChange(e.target.value || undefined)}
        >
          <option value="">A IA decide</option>
          {spec.options.map((o) => (
            <option key={o} value={o}>
              {o}
            </option>
          ))}
        </select>
      </label>
    );
  }
  return (
    <label className="field">
      <span className="field-label mono">{spec.name}</span>
      <input
        className="input"
        type={spec.kind === "range" ? "number" : "text"}
        min={spec.min ?? undefined}
        max={spec.max ?? undefined}
        value={value === undefined || value === null ? "" : String(value)}
        placeholder="A IA decide"
        onChange={(e) =>
          onChange(
            e.target.value === ""
              ? undefined
              : spec.kind === "range"
                ? Number(e.target.value)
                : e.target.value,
          )
        }
      />
    </label>
  );
}

function ModelCard(props: { m: MediaModel; on: boolean; onPick: () => void }) {
  const { m, on, onPick } = props;
  const { vendor, short } = split(m);
  const caps = capabilities(m);
  return (
    <button
      type="button"
      className={"pn-tile md-card" + (on ? " on" : "")}
      aria-pressed={on}
      aria-label={`${m.name}${m.priceHint ? `, ${m.priceHint}` : ""}${on ? ", selecionado" : ""}`}
      title={m.description || m.id}
      onClick={onPick}
    >
      <span className="pn-tile-head">
        <Logo id={m.id} size={32} />
        <span className="pn-body">
          <span className="pn-title">{short}</span>
          <span className="pn-sub">{vendor}</span>
        </span>
        {on ? (
          <span className="md-check" aria-hidden>
            <Check size={14} strokeWidth={2.5} />
          </span>
        ) : m.isNew ? (
          <span className="badge ok">Novo</span>
        ) : null}
      </span>
      <span className="md-price">{m.priceHint || "Preço não informado pelo OpenRouter"}</span>
      {caps.length ? (
        <span className="pn-tile-foot">
          {caps.map((c) => (
            <span key={c} className="badge">
              {c}
            </span>
          ))}
        </span>
      ) : null}
    </button>
  );
}

export function MediaTab() {
  const [kind, setKind] = useState<string>("image");
  const [models, setModels] = useState<MediaModel[] | null>(null);
  const [query, setQuery] = useState("");
  const [vendor, setVendor] = useState("");
  const [prompt, setPrompt] = useState("");
  const [defaults, setDefaults] = useState<Record<string, unknown>>({});
  const [references, setReferences] = useState("");
  const [key, setKey] = useState<MediaKey | null>(null);
  const [keyInput, setKeyInput] = useState("");
  const [slot, setSlot] = useState<MediaSlot | null>(null);
  const [files, setFiles] = useState<MediaFile[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    api
      .mediaKey()
      .then(setKey)
      .catch((e) => setError(String(e)));
    api
      .mediaSettings()
      .then(setSlot)
      .catch((e) => setError(String(e)));
  }, []);

  // The whole list (a few dozen, newest first); search and vendor filter
  // run here, so typing never waits on the network.
  useEffect(() => {
    let live = true;
    setModels(null);
    api
      .mediaModels(kind)
      .then((p) => {
        if (!live) return;
        setModels(p.models);
        setError(null);
      })
      .catch((e) => {
        if (!live) return;
        setModels([]);
        setError(String(e));
      });
    return () => {
      live = false;
    };
  }, [kind]);

  const chosenId = slot ? (kind === "image" ? slot.image : slot.video) : "";
  const savedDefaults = useMemo(
    () => (slot ? (kind === "image" ? slot.imageParams : slot.videoParams) : {}),
    [slot, kind],
  );
  useEffect(() => setDefaults({ ...savedDefaults }), [savedDefaults]);

  const current = models?.find((m) => m.id === chosenId) ?? null;
  const vendors = useMemo(() => {
    const seen = new Map<string, string>();
    for (const m of models ?? []) if (!seen.has(vendorKey(m))) seen.set(vendorKey(m), split(m).vendor);
    return [...seen.entries()].sort((a, b) => a[1].localeCompare(b[1]));
  }, [models]);
  const shown = useMemo(() => {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    return (models ?? []).filter(
      (m) =>
        (!vendor || vendorKey(m) === vendor) &&
        words.every((w) => `${m.id} ${m.name}`.toLowerCase().includes(w)),
    );
  }, [models, query, vendor]);

  const save = (model: string, params: Record<string, unknown>, text: string) =>
    api
      .mediaSaveSlot({ kind, model, params })
      .then(() => api.mediaSettings().then(setSlot))
      .then(() => setNotice(text))
      .catch((e) => setError(String(e)));

  // Choosing is saving: the chat uses this model from the next message. The
  // defaults belong to one model, so a new model starts without them.
  const pick = (m: MediaModel) => {
    if (m.id === chosenId) return;
    setError(null);
    void save(m.id, {}, `${split(m).short} é o modelo de ${kind === "image" ? "imagem" : "vídeo"} da IA.`);
  };

  const clean = (v: Record<string, unknown>) =>
    Object.fromEntries(Object.entries(v).filter(([, x]) => x !== undefined && x !== ""));

  const generate = async () => {
    if (!current) {
      setError("Escolha um modelo.");
      return;
    }
    if (!prompt.trim()) {
      setError("Escreva o prompt.");
      return;
    }
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const list = await api.mediaGenerate({
        kind,
        model: current.id,
        prompt,
        params: clean(defaults),
        references: references
          .split("\n")
          .map((r) => r.trim())
          .filter(Boolean),
      });
      setFiles((prev) => [...list, ...prev]);
      setNotice(list.length === 1 ? "1 arquivo gerado." : `${list.length} arquivos gerados.`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const KindIcon = kind === "image" ? ImageIcon : Film;
  const kindWord = kind === "image" ? "imagem" : "vídeo";
  const used = key?.usageUsd ?? null;
  const limit = key?.limitUsd ?? null;
  const share = used !== null && limit ? Math.min(100, (used / limit) * 100) : null;
  const pinnedUnknown = !!chosenId && models !== null && !current;

  return (
    <div className="page pn">
      {error ? (
        <div className="toast bad" role="alert">
          <CircleAlert size={16} strokeWidth={1.75} />
          <span>{error}</span>
        </div>
      ) : null}
      {notice ? (
        <div className="toast" role="status">
          <CircleCheck size={16} strokeWidth={1.75} />
          <span>{notice}</span>
        </div>
      ) : null}

      <Card
        title="Chave do OpenRouter"
        icon={KeyRound}
        description="Imagens e vídeos são gerados pelo OpenRouter, cobrados na sua conta."
        actions={
          <span className={"badge" + (key?.source ? " ok" : " warn")}>
            <span className={"dot" + (key?.source ? " ok" : " warn")} />
            {key?.source ? (SOURCE[key.source] ?? key.source) : "Sem chave"}
          </span>
        }
      >
        {key?.source ? (
          <div className="pn-usage">
            <div className="pn-inline">
              <span className="setting-label">Uso</span>
              {key.label ? <span className="badge mono">{key.label}</span> : null}
              <span className="pn-spacer" />
              <span className="pn-num">
                {money(used)}
                <span className="pn-hint"> de {money(limit)}</span>
              </span>
            </div>
            {share !== null ? (
              <div
                className={"pn-meter" + (share >= 90 ? " danger" : share >= 70 ? " warn" : "")}
                role="meter"
                aria-label="Uso da chave"
                aria-valuenow={Math.round(share)}
                aria-valuemin={0}
                aria-valuemax={100}
              >
                <span style={{ width: `${share}%` }} />
              </div>
            ) : null}
          </div>
        ) : null}
        <div className="pn-inline">
          <span className="pn-affix pn-spacer">
            <span className="pn-affix-icon">
              <KeyRound size={16} strokeWidth={1.75} />
            </span>
            <input
              className="input"
              type="password"
              value={keyInput}
              placeholder={key?.source ? "Colar outra chave para trocar" : "sk-or-…"}
              aria-label="Chave do OpenRouter"
              onChange={(e) => setKeyInput(e.target.value)}
            />
          </span>
          <button
            className="btn"
            onClick={() =>
              void api
                .mediaKey(true)
                .then(setKey)
                .catch((e) => setError(String(e)))
            }
          >
            <RefreshCw size={16} strokeWidth={1.75} /> Testar
          </button>
          <button
            className="btn btn-primary"
            disabled={!keyInput.trim()}
            onClick={() =>
              void api
                .mediaSaveKey(keyInput)
                .then(() => {
                  setKeyInput("");
                  return api.mediaKey(true).then(setKey);
                })
                .then(() => setNotice("Chave guardada no cofre do sistema."))
                .catch((e) => setError(String(e)))
            }
          >
            <Save size={16} strokeWidth={1.75} /> Guardar
          </button>
        </div>
      </Card>

      <Card
        title="Modelo"
        icon={KindIcon}
        description={`Você escolhe o modelo; duração, proporção e resolução a IA define pelo que você pedir no chat.`}
        actions={
          <Segmented
            label="Tipo de mídia"
            value={kind}
            options={KINDS}
            onPick={(v) => {
              setKind(v);
              setQuery("");
              setVendor("");
            }}
          />
        }
      >
        <div className={"md-current" + (current ? " on" : "")} role="status" aria-live="polite">
          {current ? (
            <>
              <Logo id={current.id} size={36} />
              <span className="pn-body">
                <span className="md-current-label">Modelo atual de {kindWord}</span>
                <span className="md-current-name">
                  {split(current).short}
                  <span className="pn-hint"> · {split(current).vendor}</span>
                </span>
                <span className="md-current-price">
                  {current.priceHint || "Preço não informado pelo OpenRouter"}
                </span>
              </span>
              <span className="badge accent">
                <Check size={12} strokeWidth={2.5} /> Em uso pela IA
              </span>
            </>
          ) : (
            <>
              <span className="md-current-empty">
                <KindIcon size={18} strokeWidth={1.75} />
              </span>
              <span className="pn-body">
                <span className="md-current-label">Modelo atual de {kindWord}</span>
                <span className="md-current-name">
                  {pinnedUnknown ? chosenId : "Nenhum escolhido"}
                </span>
                <span className="md-current-price">
                  {pinnedUnknown
                    ? "Este modelo não está mais na lista do OpenRouter; escolha outro."
                    : `Sem modelo, a IA não oferece geração de ${kindWord}.`}
                </span>
              </span>
            </>
          )}
        </div>

        <div className="pn-toolbar">
          <label className="pn-affix">
            <span className="pn-affix-icon">
              <Search size={16} strokeWidth={1.75} />
            </span>
            <input
              className="input"
              value={query}
              placeholder="Buscar modelo"
              aria-label="Buscar modelo"
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          <select
            className="select md-vendor"
            value={vendor}
            aria-label="Fornecedor"
            onChange={(e) => setVendor(e.target.value)}
          >
            <option value="">Todos os fornecedores</option>
            {vendors.map(([k, label]) => (
              <option key={k} value={k}>
                {label}
              </option>
            ))}
          </select>
          {models ? (
            <span className="pn-hint">
              {shown.length} de {models.length}
            </span>
          ) : null}
        </div>

        {models === null ? (
          <div className="pn-loading">
            <span className="spinner" /> Lendo os modelos e preços do OpenRouter…
          </div>
        ) : null}
        {models !== null && shown.length === 0 ? (
          <EmptyState icon={ImageOff} title="Nenhum modelo" text="Nada corresponde à busca." />
        ) : null}
        {shown.length ? (
          <div className="pn-scroll md-scroll">
            <div className="pn-grid wide" role="group" aria-label={`Modelos de ${kindWord}, mais recentes primeiro`}>
              {shown.map((m) => (
                <ModelCard key={m.id} m={m} on={m.id === chosenId} onPick={() => pick(m)} />
              ))}
            </div>
          </div>
        ) : null}

        {current && current.params.length ? (
          <details className="sheet-adv">
            <summary>
              <ChevronRight size={14} strokeWidth={2} /> Avançado: padrões opcionais
            </summary>
            <p className="pn-hint md-adv-hint">
              Só valem quando o pedido não diz. Deixe em “A IA decide” para ela escolher.
            </p>
            <div className="pn-row2">
              {current.params.map((p) => (
                <ParamField
                  key={p.name}
                  spec={p}
                  value={defaults[p.name]}
                  onChange={(v) => setDefaults((prev) => ({ ...prev, [p.name]: v }))}
                />
              ))}
            </div>
            <div className="pn-detail-foot">
              <button
                className="btn"
                onClick={() => void save(current.id, clean(defaults), "Padrões salvos.")}
              >
                <Save size={16} strokeWidth={1.75} /> Salvar padrões
              </button>
            </div>
          </details>
        ) : null}
      </Card>

      {current ? (
        <Card
          title={`Testar ${split(current).short}`}
          icon={WandSparkles}
          description="Uma geração direta, fora do chat. Gera custo."
        >
          <div className="pn-form">
            <label className="field">
              <span className="field-label">Prompt</span>
              <textarea
                className="input"
                rows={3}
                value={prompt}
                placeholder="Descreva o que quer ver."
                onChange={(e) => setPrompt(e.target.value)}
              />
            </label>
            {current.maxReferences > 0 ? (
              <label className="field">
                <span className="field-label">Referências</span>
                <textarea
                  className="input mono"
                  rows={2}
                  value={references}
                  placeholder="C:/imagens/referencia.png"
                  onChange={(e) => setReferences(e.target.value)}
                />
                <span className="field-hint">Uma por linha, até {current.maxReferences}.</span>
              </label>
            ) : null}
            <div className="pn-detail-foot">
              <button className="btn btn-primary" disabled={busy} onClick={() => void generate()}>
                {busy ? <span className="spinner" /> : <WandSparkles size={16} strokeWidth={1.75} />}
                {busy ? "Gerando…" : "Gerar"}
              </button>
            </div>
          </div>
        </Card>
      ) : null}

      <Card title="Saída" icon={FolderOpen} description="Onde os arquivos gerados são gravados.">
        <Row label="Pasta de saída">
          <span className="mono-path" title={slot?.outputDir}>
            {slot?.outputDir ?? "…"}
          </span>
          <button
            className="btn btn-sm"
            onClick={() => void api.mediaOpenDir().catch((e) => setError(String(e)))}
          >
            <FolderOpen size={14} strokeWidth={2} /> Abrir
          </button>
        </Row>
        {files.length ? (
          <div className="pn-list">
            {files.map((f) => (
              <div key={f.path} className="pn-item">
                <span className="pn-ico">
                  {f.mediaType.startsWith("video") ? (
                    <Film size={16} strokeWidth={1.75} />
                  ) : (
                    <ImageIcon size={16} strokeWidth={1.75} />
                  )}
                </span>
                <span className="pn-body">
                  <span className="pn-title">{f.path.split(/[\\/]/).pop()}</span>
                  <span className="pn-sub">
                    {f.mediaType} · {(f.bytes / 1024).toFixed(0)} kB
                    {f.costUsd !== null ? ` · ${money(f.costUsd)}` : ""}
                  </span>
                </span>
                <button
                  className="icon-btn sm"
                  title="Abrir"
                  aria-label="Abrir arquivo"
                  onClick={() => void api.mediaOpen(f.path).catch((e) => setError(String(e)))}
                >
                  <ExternalLink size={14} strokeWidth={2} />
                </button>
              </div>
            ))}
          </div>
        ) : null}
      </Card>
    </div>
  );
}
