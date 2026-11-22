import { useEffect, useState } from "react";
import { Save, Trash2, X } from "lucide-react";
import { api, type AgentView } from "../lib/api";
import { tr, useLang } from "../lib/i18n";
import { BOT_EMBLEMS, BotEmblem, emblemForAgent } from "./BotEmblem";

export function BotModal(props: {
  open: boolean;
  editing: AgentView | null;
  models: { value: string; label: string }[];
  onClose: () => void;
  onSaved: (agents: AgentView[], savedName?: string) => void;
}) {
  const lang = useLang();
  const isEdit = Boolean(props.editing);

  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [icon, setIcon] = useState("bot");
  const [model, setModel] = useState("");
  const [tools, setTools] = useState("");
  const [quickPromptsText, setQuickPromptsText] = useState("");
  const [instructions, setInstructions] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!props.open) return;
    setError(null);
    if (props.editing) {
      const b = props.editing;
      setName(b.name);
      setDescription(b.description);
      setIcon(b.icon || emblemForAgent(b.name));
      setModel(b.model ?? "");
      setTools((b.tools ?? []).join(", "));
      setQuickPromptsText((b.quickPrompts ?? []).join("\n"));
      setInstructions(b.instructions);
    } else {
      setName("");
      setDescription("");
      setIcon("bot");
      setModel("");
      setTools("");
      setQuickPromptsText("");
      setInstructions("");
    }
  }, [props.open, props.editing]);

  useEffect(() => {
    if (!props.open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") props.onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [props.open, props.onClose]);

  if (!props.open) return null;

  const handleSave = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    const cleanName = name.trim();
    if (!cleanName) {
      setError(tr(lang, "Informe o nome do bot (ex: mikrotik-ops).", "Enter a bot name (e.g. mikrotik-ops)."));
      return;
    }
    if (!instructions.trim()) {
      setError(tr(lang, "Escreva as instruções / persona do bot.", "Write the bot instructions / persona."));
      return;
    }
    setSaving(true);
    try {
      const quickPrompts = quickPromptsText
        .split("\n")
        .map((s) => s.trim())
        .filter(Boolean);
      const form = {
        name: cleanName,
        description: description.trim(),
        instructions: instructions.trim(),
        tools: tools.trim(),
        model: model.trim(),
        icon,
        quickPrompts,
      };
      const updated = isEdit
        ? await api.agentSave(form)
        : await api.agentCreate(form);
      props.onSaved(updated, cleanName);
      props.onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!props.editing) return;
    setSaving(true);
    setError(null);
    try {
      const updated = await api.agentDelete(props.editing.name);
      props.onSaved(updated);
      props.onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="bot-modal-backdrop" onMouseDown={props.onClose}>
      <div
        className="bot-modal"
        role="dialog"
        aria-modal="true"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="bot-modal-head">
          <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
            <BotEmblem icon={icon} size={34} status="on" />
            <div>
              <h3>
                {isEdit
                  ? tr(lang, `Editar @${props.editing!.name}`, `Edit @${props.editing!.name}`)
                  : tr(lang, "Novo Bot Especialista", "New Specialist Bot")}
              </h3>
              <p>
                {tr(
                  lang,
                  "Defina emblema, persona, prompts rápidos e modelo padrão.",
                  "Configure emblem, persona, quick prompts, and default model.",
                )}
              </p>
            </div>
          </div>
          <button type="button" className="icon-btn" onClick={props.onClose} aria-label="Fechar">
            <X size={16} />
          </button>
        </header>

        <form className="bot-modal-form" onSubmit={handleSave}>
          <div className="bot-modal-grid">
            <label className="bot-field">
              <span>{tr(lang, "Identificador (@nome)", "Identifier (@name)")}</span>
              <input
                value={name}
                disabled={isEdit}
                onChange={(e) => setName(e.target.value.replace(/[^a-zA-Z0-9_-]/g, "-"))}
                placeholder="ex: mikrotik-ops"
                required
              />
            </label>
            <label className="bot-field">
              <span>{tr(lang, "Modelo preferido (opcional)", "Preferred model (optional)")}</span>
              <select value={model} onChange={(e) => setModel(e.target.value)}>
                <option value="">
                  {tr(lang, "Usar modelo ativo do Zerith", "Use active Zerith model")}
                </option>
                {props.models.map((m) => (
                  <option key={m.value} value={m.value}>
                    {m.label}
                  </option>
                ))}
              </select>
            </label>
          </div>

          <label className="bot-field">
            <span>{tr(lang, "Descrição curta", "Short description")}</span>
            <input
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder={tr(
                lang,
                "Ex: Especialista em BGP, OSPF, Mikrotik e diagnósticos FTTH",
                "e.g. Specialist in BGP, OSPF, Mikrotik and FTTH diagnostics",
              )}
            />
          </label>

          <div className="bot-field">
            <span>{tr(lang, "Emblema visual", "Visual emblem")}</span>
            <div className="bot-emblem-picker">
              {BOT_EMBLEMS.map((em) => (
                <button
                  key={em.id}
                  type="button"
                  className={"bot-emblem-opt" + (icon === em.id ? " on" : "")}
                  onClick={() => setIcon(em.id)}
                  title={em.label}
                >
                  <BotEmblem icon={em.id} size={28} />
                </button>
              ))}
            </div>
          </div>

          <label className="bot-field">
            <span>{tr(lang, "Instruções / Persona (System Prompt)", "Instructions / Persona (System Prompt)")}</span>
            <textarea
              rows={5}
              value={instructions}
              onChange={(e) => setInstructions(e.target.value)}
              placeholder={tr(
                lang,
                "Descreva como esse bot deve pensar, responder e quais ferramentas priorizar...",
                "Describe how this bot should reason, respond, and which tools to prioritize...",
              )}
              required
            />
          </label>

          <div className="bot-modal-grid">
            <label className="bot-field">
              <span>{tr(lang, "Prompts rápidos (1 por linha)", "Quick prompts (1 per line)")}</span>
              <textarea
                rows={3}
                value={quickPromptsText}
                onChange={(e) => setQuickPromptsText(e.target.value)}
                placeholder={tr(
                  lang,
                  "Auditar interfaces e latência\nResumir logs críticos",
                  "Audit interfaces and latency\nSummarize critical logs",
                )}
              />
            </label>
            <label className="bot-field">
              <span>{tr(lang, "Permitir só ferramentas (opcional)", "Allow only tools (optional)")}</span>
              <textarea
                rows={3}
                value={tools}
                onChange={(e) => setTools(e.target.value)}
                placeholder={tr(
                  lang,
                  "Vazio = todas as ferramentas\nEx: Read, Grep, shell, workflow_run",
                  "Empty = all tools\ne.g. Read, Grep, shell, workflow_run",
                )}
              />
            </label>
          </div>

          {error ? <div className="bot-modal-err">{error}</div> : null}

          <footer className="bot-modal-foot">
            {isEdit && props.editing?.layer === "user" ? (
              <button
                type="button"
                className="btn danger"
                disabled={saving}
                onClick={handleDelete}
              >
                <Trash2 size={14} />
                <span>{tr(lang, "Excluir bot", "Delete bot")}</span>
              </button>
            ) : (
              <span />
            )}
            <div style={{ display: "flex", gap: 8 }}>
              <button type="button" className="btn" onClick={props.onClose} disabled={saving}>
                {tr(lang, "Cancelar", "Cancel")}
              </button>
              <button type="submit" className="btn btn-primary" disabled={saving}>
                <Save size={14} />
                <span>
                  {saving
                    ? tr(lang, "Salvando…", "Saving…")
                    : tr(lang, "Salvar Bot", "Save Bot")}
                </span>
              </button>
            </div>
          </footer>
        </form>
      </div>
    </div>
  );
}
