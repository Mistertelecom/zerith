import { useEffect, useMemo, useState } from "react";
import {
  CircleAlert,
  CircleCheck,
  ExternalLink,
  Film,
  FolderOpen,
  Image as ImageIcon,
  ImageOff,
  KeyRound,
  Pin,
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

/// One control per parameter the model declares. Values start at the saved
/// slot, so reopening the panel shows what will actually be sent.
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
        <Switch label={spec.name} checked={value === true} onChange={(v) => onChange(v)} />
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
          onChange={(e) => onChange(e.target.value)}
        >
          <option value="">—</option>
          {spec.options.map((o) => (
            <option key={o} value={o}>
              {o}
            </option>
          ))}
        </select>
      </label>
    );
  }
  if (spec.kind === "range") {
    return (
      <label className="field">
        <span className="field-label mono">{spec.name}</span>
        <input
          className="input"
          type="number"
          min={spec.min ?? undefined}
          max={spec.max ?? undefined}
          value={value === undefined || value === null ? "" : String(value)}
          onChange={(e) =>
            onChange(e.target.value === "" ? undefined : Number(e.target.value))
          }
        />
        {spec.min !== null && spec.max !== null ? (
          <span className="field-hint">
            De {spec.min} a {spec.max}.
          </span>
        ) : null}
      </label>
    );
  }
  return (
    <label className="field">
      <span className="field-label mono">{spec.name}</span>
      <input
        className="input"
        value={value === undefined || value === null ? "" : String(value)}
        onChange={(e) => onChange(e.target.value)}
      />
    </label>
  );
}

export function MediaTab() {
  const [kind, setKind] = useState<string>("image");
  const [models, setModels] = useState<MediaModel[] | null>(null);
  const [query, setQuery] = useState("");
  const [model, setModel] = useState("");
  const [prompt, setPrompt] = useState("");
  const [values, setValues] = useState<Record<string, unknown>>({});
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

  useEffect(() => {
    let live = true;
    setModels(null);
    api
      .mediaModels(kind, query)
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
  }, [kind, query]);

  // Picking a model restores the parameters saved for it, if it is the one in
  // the slot; otherwise the descriptor defaults apply.
  const pick = (m: MediaModel) => {
    setModel(m.id);
    const saved =
      slot && ((kind === "image" ? slot.image : slot.video) === m.id)
        ? kind === "image"
          ? slot.imageParams
          : slot.videoParams
        : {};
    setValues({ ...saved });
  };

  const current = useMemo(
    () => models?.find((m) => m.id === model) ?? null,
    [models, model],
  );

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
        params: Object.fromEntries(
          Object.entries(values).filter(([, v]) => v !== undefined && v !== ""),
        ),
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

  const pinned = slot ? (kind === "image" ? slot.image : slot.video) : "";
  const KindIcon = kind === "image" ? ImageIcon : Film;
  const used = key?.usageUsd ?? null;
  const limit = key?.limitUsd ?? null;
  const share = used !== null && limit ? Math.min(100, (used / limit) * 100) : null;

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
        description={
          models
            ? `${models.length} ${models.length === 1 ? "modelo" : "modelos"} de ${kind === "image" ? "imagem" : "vídeo"}`
            : undefined
        }
        actions={
          <Segmented
            label="Tipo de mídia"
            value={kind}
            options={KINDS}
            onPick={(v) => {
              setKind(v);
              setModel("");
              setValues({});
            }}
          />
        }
      >
        <label className="pn-affix">
          <span className="pn-affix-icon">
            <Search size={16} strokeWidth={1.75} />
          </span>
          <input
            className="input"
            value={query}
            placeholder="Filtrar modelos"
            aria-label="Filtrar modelos"
            onChange={(e) => setQuery(e.target.value)}
          />
        </label>
        {models === null ? (
          <div className="pn-loading">
            <span className="spinner" /> Lendo os modelos…
          </div>
        ) : null}
        {models?.length === 0 ? (
          <EmptyState icon={ImageOff} title="Nenhum modelo" text="Nada corresponde ao filtro." />
        ) : null}
        {models?.length ? (
          <div className="pn-scroll">
            <div className="pn-grid wide">
              {models.map((m) => (
                <button
                  key={m.id}
                  className={"pn-tile" + (model === m.id ? " on" : "")}
                  aria-pressed={model === m.id}
                  onClick={() => pick(m)}
                >
                  <span className="pn-tile-head">
                    <Logo id={m.id} size={28} />
                    <span className="pn-body">
                      <span className="pn-title">{m.name}</span>
                      <span className="pn-sub mono">{m.id}</span>
                    </span>
                  </span>
                  <span className="pn-tile-foot">
                    <span className="badge mono">{m.priceHint}</span>
                    {pinned === m.id ? (
                      <span className="badge accent">
                        <Pin size={12} strokeWidth={2} /> Fixado
                      </span>
                    ) : null}
                  </span>
                </button>
              ))}
            </div>
          </div>
        ) : null}
      </Card>

      {current ? (
        <Card
          title={`Gerar com ${current.name}`}
          icon={WandSparkles}
          description={current.description || undefined}
          actions={<span className="badge mono">{current.priceHint}</span>}
        >
          <div className="pn-form">
            <label className="field">
              <span className="field-label">Prompt</span>
              <textarea
                className="input"
                rows={4}
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
            {current.params.length ? (
              <div className="pn-row2">
                {current.params.map((p) => (
                  <ParamField
                    key={p.name}
                    spec={p}
                    value={values[p.name]}
                    onChange={(v) => setValues((prev) => ({ ...prev, [p.name]: v }))}
                  />
                ))}
              </div>
            ) : null}
            <div className="pn-detail-foot">
              <button
                className="btn"
                disabled={busy}
                onClick={() =>
                  void api
                    .mediaSaveSlot({ kind, model: current.id, params: values })
                    .then(() => api.mediaSettings().then(setSlot))
                    .then(() => setNotice("Modelo e parâmetros fixados."))
                    .catch((e) => setError(String(e)))
                }
              >
                <Pin size={16} strokeWidth={1.75} /> Fixar para a IA
              </button>
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
