// Dev-only entry for `/preview.html`: renders the real App over the fake core
// and drives it to one scene, so a headless browser can photograph it.
//   ?scene=empty|chat|approval|question|settings|workflows  &tab=Contas  &open=anthropic  &theme=light
//   Servidores: ?scene=settings&tab=Servidores; editor remoto: ?scene=workflows[&runs=1|&node=n2]
//   Mídia: ?scene=settings&tab=Mídia (imagem) or &tab=Mídia&click=Vídeo (vídeo)
//   Canais/alerta: ?scene=channels (&alert=1)
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { emit } from "@tauri-apps/api/event";
import { installMock } from "./mock";
import App from "../App";
import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
import "../styles/index.css";

const q = new URLSearchParams(location.search);
const scene = q.get("scene") ?? "empty";
const theme = q.get("theme") ?? "dark";
// The scene drivers click Portuguese labels (Ajustes, Mídia): pt-BR unless &lang= says otherwise.
try {
  localStorage.setItem("zerith.lang", q.get("lang") ?? "pt-BR");
} catch {
  /* no storage: the app falls back to English */
}
installMock(theme);

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** First clickable element under `scope` whose visible text or label is `text`. */
function byText(text: string, scope = "body"): HTMLElement | null {
  const all = [...document.querySelectorAll<HTMLElement>(`${scope} :is(button, [role=tab], a)`)];
  return (
    all.find((b) => b.getAttribute("aria-label") === text) ??
    all.find((b) => b.innerText.trim() === text) ??
    all.find((b) => b.innerText.trim().startsWith(text)) ??
    null
  );
}

async function until(fn: () => HTMLElement | null, ms = 3000): Promise<HTMLElement | null> {
  for (let t = 0; t < ms; t += 50) {
    const el = fn();
    if (el) return el;
    await wait(50);
  }
  return null;
}

const turn = (ev: unknown) => emit("turn:event", ["s1", ev]);

async function drive() {
  if (scene === "settings") {
    (await until(() => byText("Ajustes")))?.click();
    const tab = q.get("tab");
    // Scoped: the sidebar also has a "Memória" button (it pauses memory).
    if (tab) (await until(() => byText(tab, ".settings-nav")))?.click();
    // ?open=<catalogue id>: the Contas connect sheet for that card
    const open = q.get("open");
    if (open) (await until(() => document.querySelector<HTMLElement>(`[data-entry="${open}"]`)))?.click();
    // Skills: &sub=Catálogo opens a sub-tab, &repo=owner/name opens a repository.
    const sub = q.get("sub");
    if (sub) (await until(() => byText(sub, ".pn-toolbar")))?.click();
    // &click=<button text>: one more click in the panel (Importar do Hermes, Testar codebase-memory-mcp).
    for (const c of q.getAll("click")) {
      (await until(() => byText(c, ".settings-body")))?.click();
      await wait(300);
    }
    const repo = q.get("repo");
    if (repo) (await until(() => byText(`Abrir ${repo}`)))?.click();
    // Delegação: &pick=Modelo: Principal opens that picker; &scroll=<px|bottom> scrolls the body.
    const pick = q.get("pick");
    if (pick) (await until(() => byText(pick)))?.click();
    const scroll = q.get("scroll");
    const body = scroll ? await until(() => document.querySelector<HTMLElement>(".settings-body")) : null;
    if (body) {
      await wait(300);
      body.scrollTop = scroll === "bottom" ? body.scrollHeight : Number(scroll);
    }
    return;
  }
  // Automações com workflows em servidor: ?scene=workflows (editor com
  // "Executar em", lista com saúde remota); &runs=1 abre as execuções do
  // servidor; &node=n2 seleciona o nó SSH (segredos no servidor).
  if (scene === "workflows") {
    (await until(() => byText("Automações", ".side-foot")))?.click();
    const card = await until(() => document.querySelector<HTMLElement>(".wf-roster-card"));
    card?.click();
    if (q.get("runs")) (await until(() => byText("Execuções", ".wf-insp-tabs")))?.click();
    const node = q.get("node");
    if (node) {
      await wait(400);
      document.querySelectorAll<HTMLElement>(".wf-node")[Number(node.replace(/\D/g, "")) - 1]?.click();
    }
    return;
  }
  if (scene === "empty") return;
  // Automações: ?scene=channels abre o modal de canais e lista os grupos da
  // Evolution; &alert=1 fica no editor (seção "Alerta de falha").
  if (scene === "channels") {
    (await until(() => byText("Automações")))?.click();
    if (q.get("alert")) return;
    (await until(() => byText("Canais")))?.click();
    (await until(() => byText("Escolher grupo")))?.click();
    return;
  }

  (await until(() => byText("Migrar o cliente HTTP do provedor OpenAI")))?.click();
  await wait(300);
  await turn({ type: "route", accountId: "claude-assinatura", provider: "anthropic", model: "claude-sonnet-5-5", attempts: 1 });
  await turn({
    type: "tasks",
    items: [
      ["Ler openai.rs e mapear os usos", "completed"],
      ["Trocar o cliente mantendo a API", "in_progress"],
      ["Rodar cargo test -p zerith-providers", "pending"],
    ].map(([subject, status], i) => ({
      id: i + 1,
      session_id: "s1",
      subject,
      description: "",
      status,
      blocks: [],
      blocked_by: [],
      created_at: 0,
      updated_at: 0,
    })),
  });
  await turn({ type: "reasoning", text: "Preciso ver onde o agente é criado antes de editar." });
  await turn({ type: "toolCall", id: "t1", name: "read_file", args: '{"path":"crates/zerith-providers/src/openai.rs"}' });
  await turn({ type: "toolResult", id: "t1", name: "read_file", ok: true, preview: "pub struct Client {\n    cfg: ProviderConfig,\n    agent: ureq::Agent,\n}" });
  await turn({ type: "toolCall", id: "t2", name: "grep", args: '{"pattern":"AgentBuilder"}' });
  await turn({ type: "toolResult", id: "t2", name: "grep", ok: true, preview: "openai.rs:74  let agent = ureq::AgentBuilder::new()" });
  await turn({ type: "toolCall", id: "t3", name: "bash", args: '{"command":"cargo test -p zerith-providers"}' });
  await turn({
    type: "toolResult",
    id: "t3",
    name: "bash",
    ok: false,
    preview:
      "exit 1\n[o comando FALHOU]\nerror[E0063]: missing field `auth` in initializer of `Client`\n  --> crates/zerith-providers/src/openai.rs:41:9",
  });
  await turn({ type: "notice", text: "Contexto em **72%**: compacte com `/compact` se a conversa crescer." });
  await turn({ type: "subagent", id: "sa1", name: "explorer: mapear usos de `ureq`", state: "running", summary: "Lendo `codex.rs` e `http.rs`…", model: "anthropic:claude-haiku-4-5" });
  await turn({ type: "delta", text: "Achei o ponto: o agente é recriado a cada chamada. " });
  await turn({ type: "delta", text: "Vou extrair `http::client` e reaproveitar a conexão." });
  await turn({ type: "usage", input: 41_200, output: 2_350 });
  await turn({ type: "saved", tool: "bash", before: 9_000, after: 1_200 });

  // &tab=expand (shots.sh: `chat:expand`): abre raciocínio e ferramentas do
  // histórico; &scroll=<px|top> rola a transcrição para fotografar o meio.
  if (q.get("tab") === "expand") {
    await wait(300);
    document.querySelectorAll<HTMLElement>(".think-head, .tool-head:not(:disabled), .msg-user-more").forEach((b) => b.click());
  }
  const chatScroll = q.get("scroll") ?? (q.get("tab") === "top" ? "0" : null);
  if (chatScroll) {
    await wait(400);
    const t = document.querySelector<HTMLElement>(".transcript");
    if (t) t.scrollTop = Number(chatScroll);
  }

  if (scene === "approval") {
    await turn({ type: "approval", id: 7, tool: "bash", summary: "cargo test -p zerith-providers --all-features", always: "shell(cargo test *)" });
  }
  // The turn ended with a monitor still running; it prints, and the app
  // starts a turn on its own (`turn:wake`) that answers the notice.
  if (scene === "wake") {
    const done = { type: "done", stored: 9, text: "", promptTokens: 0, completionTokens: 0, savedTokens: 0, modelUsed: null };
    await turn(done);
    await emit("turn:done", "s1");
    await wait(400);
    await emit("turn:wake", [
      "s1",
      "[Monitor job-3: deploy em produção]\nDEPLOY CONCLUIDO: 4/4 etapas ok\n\n[Monitor job-3: deploy em produção] terminou (exit 0).\n[aviso automático do Zerith, não escrito pelo usuário]",
    ]);
    await wait(200);
    await turn({ type: "delta", text: "O deploy terminou: as 4 etapas passaram e o monitor saiu com exit 0." });
    await turn({ ...done, stored: 10 });
    await emit("turn:done", "s1");
  }
  // &learned=1: the background review's note under the answer.
  if (q.get("learned")) {
    await turn({ type: "done", stored: 9, text: "", promptTokens: 0, completionTokens: 0, savedTokens: 0, modelUsed: null });
    await emit("turn:learned", [
      "s1",
      [
        { kind: "memória", op: "add", text: "O usuário prefere respostas curtas", id: 4, name: null, previous: null },
        { kind: "skill", op: "create", text: "Use when cmd.exe rejects POSIX flags", id: null, name: "windows-cmd-shell", previous: null },
      ],
    ]);
  }
  if (scene === "question") {
    await turn({
      type: "question",
      id: 3,
      questions: [
        {
          header: "Escopo",
          question: "Mantenho o retry atual ou troco pela política nova?",
          multi_select: false,
          options: [
            { label: "Manter", description: "Menor diff, mesmo comportamento" },
            { label: "Trocar", description: "Backoff exponencial com jitter" },
          ],
        },
      ],
    });
  }
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
void drive();
