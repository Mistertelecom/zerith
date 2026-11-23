// Dev-only: fake Rust core so every screen can be rendered and photographed in
// a plain browser (`/preview.html?scene=...`). Never imported by index.html.
import { mockIPC } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";

const now = Math.floor(Date.now() / 1000);
const PREVIEW_IMG =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAoAAAAEsCAIAAADcmZV8AAAFXklEQVR42u3dO3LCMBiF0asMS2AbPCrb+6+Ayo8FkR24wJGJ7XPaBE2iMPPlF2Io5/M5s0opS75h4cOtb33rW9/61t/l+j8BAFYnwAAgwAAgwACAAAOAAAMAAgwAAgwACDAACDAAIMAAIMAAIMAAgAADgAADAAIMAAIMAAgwAAgwACDAACDAACDAAIAAA4AAAwACDAACDAAIMAAIMAAgwAAgwAAgwACAAAOAAAMAAgwAAgwACDAACDAAIMAAIMAAIMAAgAADgAADAAIMAAIMAAgwAAgwACDAACDAACDAAIAAA4AAAwACDAACDAAIMAAIMAAgwAAgwAAgwACAAAOAAAMAAgwAAgwACDAACDAAIMAAIMAAIMAAgAADgAADAAIMAAIMAAgwAAgwACDAACDAACDAAIAAA4AAAwACDAACDAAIMAAIMAAgwAAgwAAgwLYAAAQYAAQYABBgABBgAOBzJ1sAVfV9v90fvmkaf0EQYNiMYRj28Yu8Xi8xBgEG3f0XMVZiEGDQXSWGuIQF6ptjlhgwAcN6xnG0Cc/nM0nbtrYCTMCgvt/JMGACBuk1CoMJGNTXKAwIMKivBoMAg/pqMAgwoL4aDAIM6qvBIMCgvmgwCDAACDDs1TRNNmGhx+NhE0CAQX01GAQYAAQYMP4agkGAQX01GAQYABBgABBgiPNn4hQaBBgABBgAEGCI8+c4hQYBBgABBgAEGAD27WQLIJVfAC6lbHRP3u93KrwM3HWd5xuYgIG5fx22+98DCDCw+QzbBBBgABBgwBAMCDAACDAAIMAAIMBADv+GYBBgAECAAeMvCDCA9yCBAAMmYIgPYwA2ELCF86v6ggAD0gtxBA2oLyDAgPqCAAPqCwgwqK/6QlzCAqQXMAGD+qovCDCgviDAgPoCAgyoL8QlLEB6ARMwqK/6ggAD6gsIMKiv+oIAA+oLxCUskF7ABAyoLwgwoL6AAIP6qi8IMKC+QFzCAumVXjABA+oLCDCoLyDAgPoCAgzqC8QlLJBe6QUTMKC+gACD+qovCDCgvoAAg/oCcQkLkF7ABAzqCwgwqK/6AgIM6gsIMKiv+kJcwgKkFxBgOF59/2QF+Yc4ggb1BRBgUF9AgAFAgAHjLyDAACDAAIAAA4AAA4m3zwICDAACDIZgAAGG+g2WYUCAQYaB+DAG+I8ul8s0TU6ka+u6ziaACRgABBgABBgAEGDIOi8D24R4ARgEGAAEGAAQYIhT6Dh/BgEGAAQYAAQY4hQ6zp+dP4MAgwarLwgwAAgwYAg2/oIAgwarLwgwACDAsIbr9WoTPta2rU0AAQYNVl8QYNBg9QUBBjRYfUGAQYPVFwQY0GD1hQpOtgA+aPA4jrZCesEEDEZh9QUTMBiFpRcwAUNVt9vtsL970zSeAGAChu83eBgG3QUEGJRYd0GA4ZAlLqX0fS+6gADD2u73+8xXSynzD5//hoUPX74+EJewAECAAQABBgABBgAEGAAEGAAEGAAQYAAQYABAgAFAgAEAAQYAAQYABBgABBgABBgAEGAAEGAAQIABQIABAAEGAAEGAAQYAAQYAAQYABBgABBgAECAAUCAAQABBgABBgAEGAAEGAAEGAAQYAAQYABAgAFAgAEAAQYAAQYABBgABBgABBgAEGAAEGAAQIABQIABAAEGAAEGAAQYAAQYAAQYABBgABBgAECAAUCAAQABBgABBgAEGAAEGAAEGAAQYAAQYABAgAFAgAEAAQYAAQYABBgABBgABBgAEGAAEGAAQIABQIABAAEGAAEGAAQYAAQYAAQYABBgABBgAECAAUCAAQABBgABBgAEGAAEGAAE2BYAwPp+AU86J8jSf/szAAAAAElFTkSuQmCC";
const H = 3600;

const sessions = [
  [
    "s1",
    "Migrar o cliente HTTP do provedor OpenAI",
    120,
    "claude-sonnet-5-5",
    "anthropic",
  ],
  ["s2", "Revisar o plano de OAuth do Codex", 2 * H, "gpt-5-codex", "codex"],
  [
    "s3",
    "Diagnóstico da OLT Huawei MA5800",
    26 * H,
    "deepseek-chat",
    "deepseek",
  ],
  [
    "s4",
    "Ideias de vídeo para o canal Relatos do Escuro",
    3 * 24 * H,
    "gemini-3-pro",
    "gemini",
  ],
  [
    "s5",
    "Refatorar o loop do agente",
    9 * 24 * H,
    "claude-opus-5-5",
    "anthropic",
  ],
].map(([id, title, ago, model, provider]) => ({
  id,
  title,
  source: "native",
  source_id: null,
  source_profile: null,
  model,
  provider,
  created_at: now - (ago as number) - 600,
  updated_at: now - (ago as number),
  archived: false,
}));

const providers = [
  {
    name: "anthropic",
    kind: "anthropic",
    protocol: "anthropic",
    base_url: "https://api.anthropic.com/v1",
    default_model: "claude-sonnet-5-5",
    known_models: ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"],
    context_limit: 200000,
    timeout_ms: 60000,
    account_id: "claude-assinatura",
    has_key: true,
    auth: "oauth",
  },
  {
    name: "codex",
    kind: "codex",
    protocol: "responses",
    base_url: "https://chatgpt.com/backend-api/codex",
    default_model: "gpt-5-codex",
    known_models: ["gpt-5-codex"],
    context_limit: 400000,
    timeout_ms: 60000,
    account_id: "codex",
    has_key: true,
    auth: "oauth",
  },
  {
    name: "deepseek",
    kind: "deepseek",
    protocol: "openai",
    base_url: "https://api.deepseek.com",
    default_model: "deepseek-chat",
    known_models: ["deepseek-chat", "deepseek-reasoner"],
    context_limit: 128000,
    timeout_ms: 60000,
    account_id: null,
    has_key: false,
    auth: "api-key",
  },
];

const presets = [
  [
    "anthropic",
    "Anthropic",
    "https://api.anthropic.com/v1",
    "anthropic",
    "claude-sonnet-5-5",
  ],
  ["openai", "OpenAI", "https://api.openai.com/v1", "openai", "gpt-6"],
  [
    "deepseek",
    "DeepSeek",
    "https://api.deepseek.com",
    "openai",
    "deepseek-chat",
  ],
  [
    "gemini",
    "Gemini",
    "https://generativelanguage.googleapis.com/v1beta/openai",
    "openai",
    "gemini-3-pro",
  ],
  [
    "openrouter",
    "OpenRouter",
    "https://openrouter.ai/api/v1",
    "openai",
    "anthropic/claude-sonnet-5-5",
  ],
  ["ollama", "Ollama", "http://localhost:11434/v1", "openai", "qwen3:8b"],
  [
    "lmstudio",
    "LM Studio",
    "http://localhost:1234/v1",
    "openai",
    "local-model",
  ],
  [
    "codex",
    "Codex",
    "https://chatgpt.com/backend-api/codex",
    "responses",
    "gpt-5-codex",
  ],
].map(([kind, label, base_url, protocol, model_hint]) => ({
  kind,
  label,
  base_url,
  protocol,
  model_hint,
  needs_key: !["ollama", "lmstudio"].includes(kind),
}));

const accounts = [
  ["claude-assinatura", "anthropic", "oauth", "claude-code", 0, true, null],
  ["codex", "codex", "oauth", "codex", 1, true, null],
  ["antigravity", "antigravity", "oauth", "antigravity", 1, true, null],
  ["deepseek-trabalho", "deepseek", "api-key", null, 2, true, 2000000],
  ["openrouter-backup", "openrouter", "api-key", null, 5, false, null],
].map(([id, provider, auth, oauth_tool, priority, enabled, quota]) => ({
  id,
  provider,
  auth,
  oauth_tool,
  label: null,
  protocol: null,
  base_url: null,
  default_model: null,
  api_key_ref: null,
  priority,
  enabled,
  quota_tokens: quota,
  quota_requests: null,
  window_hours: 5,
}));

const usage = {
  accounts: [
    [
      "claude-assinatura",
      "anthropic",
      412_000,
      61_200,
      38_400,
      96,
      0,
      0,
      "claude-sonnet-5-5",
    ],
    [
      "codex",
      "codex",
      188_000,
      22_900,
      12_100,
      41,
      2,
      now * 1000 + 15 * 60_000,
      "gpt-5-codex",
    ],
    [
      "deepseek-trabalho",
      "deepseek",
      74_000,
      9_800,
      0,
      18,
      0,
      0,
      "deepseek-chat",
    ],
  ].map(([account_id, provider, p, c, s, r, rl, cd, m]) => ({
    account_id,
    provider,
    prompt_tokens: p,
    completion_tokens: c,
    total_tokens: (p as number) + (c as number),
    saved_tokens: s,
    requests: r,
    rate_limits: rl,
    cooldown_until: cd,
    last_error: rl ? "rate limited (429)" : null,
    last_model: m,
    last_used_at: now * 1000 - 300_000,
  })),
  models: [
    ["claude-sonnet-5-5", "anthropic", "main", 380_000, 80],
    ["gpt-5-codex", "codex", "subagent", 210_900, 41],
    ["claude-haiku-5-5", "anthropic", "explore", 93_200, 16],
    ["deepseek-chat", "deepseek", "main", 83_800, 18],
  ].map(([model, provider, role_kind, total_tokens, requests]) => ({
    model,
    provider,
    role_kind,
    total_tokens,
    requests,
  })),
  total_prompt_tokens: 674_000,
  total_completion_tokens: 93_900,
  total_saved_tokens: 50_500,
  total_requests: 155,
  active_account: "claude-assinatura",
  active_model: "claude-sonnet-5-5",
};

const facts = [
  [
    "preference",
    "Responde em português do Brasil, direto e sem enrolação.",
    "confirmed",
  ],
  ["preference", "Prefere tema escuro e interfaces densas.", "inferred"],
  [
    "project",
    "Zerith: harness de agente em Rust com janela Tauri 2.",
    "confirmed",
  ],
  ["project", "White Silence: survival 3D coop em Godot 4.", "confirmed"],
  [
    "instruction",
    "Em equipamento de produção, nunca executar comando sem aprovação.",
    "confirmed",
  ],
  ["decision", "OAuth antes do redesign visual.", "confirmed"],
  ["entity", "Facilnet Telecom: cliente de consultoria (ISP).", "inferred"],
].map(([kind, text, confidence], i) => ({
  id: i + 1,
  scope: "user:joao",
  kind,
  text,
  source: i % 2 ? "s2" : "s1",
  session_id: i % 2 ? "s2" : "s1",
  confidence,
  created_at: now - i * 86400,
  updated_at: now - i * 86400,
  superseded_by: null,
}));

const skills = [
  [
    "code-review",
    "Revisa um diff e aponta riscos antes do commit.",
    "enabled",
    [],
  ],
  [
    "systematic-debugging",
    "Debug em quatro fases: entender antes de corrigir.",
    "enabled",
    [],
  ],
  [
    "web-scraper",
    "Extrai dados de páginas com navegador headless.",
    "disabled",
    [{ severity: "warn", message: "executa scripts locais" }],
  ],
].map(([name, description, status, findings]) => ({
  installed: {
    name,
    description,
    license: "MIT",
    source: `github.com/zerith/skills/${name}`,
    status,
    dir: `C:/Users/Joao/AppData/Roaming/Zerith/skills/${name}`,
    installed_at: now - 5 * 86400,
    previous_dir: null,
  },
  findings,
  scripts: name === "web-scraper" ? ["scrape.py"] : [],
  inventory: [["SKILL.md", 2400]],
  body: `# ${name}\n\n${description}\n\n## Quando usar\n\nSempre que a tarefa pedir.`,
}));

// Skills tab: the bundled defaults arrive disabled; the Catálogo lists the
// skills inside a repository and installs one into this same list.
const skillView = (
  name: string,
  description: string,
  source: string,
  status: string,
) => ({
  installed: {
    name,
    description,
    license: "MIT",
    source,
    status,
    dir: `C:/Users/Joao/AppData/Roaming/Zerith/skills/${name}`,
    installed_at: now - 86400,
    previous_dir: null,
  },
  findings: [] as { severity: string; message: string }[],
  scripts: [] as string[],
  inventory: [["SKILL.md", 2400]],
  body: `# ${name}\n\n${description}\n\n## Quando usar\n\nSempre que a tarefa pedir.`,
});
const allSkills: ReturnType<typeof skillView>[] = [
  ...[
    ["docx", "Cria, lê e edita documentos Word (.docx) com formatação."],
    ["pdf", "Lê, extrai, junta, divide e preenche formulários em PDF."],
    ["xlsx", "Planilhas: fórmulas, formatação, gráficos e análise de dados."],
    [
      "frontend-design",
      "Design de interface distinto e intencional para telas novas.",
    ],
    ["skill-creator", "Cria e melhora skills e mede o desempenho delas."],
  ].map(([n, d]) =>
    skillView(n, d, `bundled:${n}`, n === "pdf" ? "enabled" : "disabled"),
  ),
  ...(skills as unknown as ReturnType<typeof skillView>[]),
];
const remoteSkills = [
  [
    "skills/algorithmic-art",
    "algorithmic-art",
    "Arte generativa com p5.js: aleatoriedade com semente, campos de fluxo e partículas.",
  ],
  [
    "skills/brand-guidelines",
    "brand-guidelines",
    "Aplica cores e tipografia oficiais da marca a qualquer artefato.",
  ],
  [
    "skills/canvas-design",
    "canvas-design",
    "Pôsteres e peças visuais em .png e .pdf a partir de uma filosofia de design.",
  ],
  [
    "skills/docx",
    "docx",
    "Cria, lê e edita documentos Word (.docx) com formatação.",
  ],
  [
    "skills/pdf",
    "pdf",
    "Lê, extrai, junta, divide e preenche formulários em PDF.",
  ],
  [
    "skills/slack-gif-creator",
    "slack-gif-creator",
    "GIFs animados otimizados para o Slack, dentro dos limites de tamanho.",
  ],
  [
    "skills/theme-factory",
    "theme-factory",
    "Dez temas prontos de cores e fontes para slides, documentos e páginas.",
  ],
].map(([path, name, description]) => ({ path, name, description }));

const catalog = {
  fetched_at: now - 3600,
  entries: [
    [
      "anthropics/skills",
      "Skills oficiais da Anthropic: docx, pdf, xlsx, pptx.",
      18200,
    ],
    ["obra/superpowers", "TDD, debugging sistemático, planos e revisão.", 9400],
    [
      "openai/codex-skills",
      "Skills do Codex CLI portadas para Agent Skills.",
      3100,
    ],
  ].map(([repo, description, stars]) => ({
    repo,
    description,
    stars,
    forks: Math.round((stars as number) / 9),
    updated_at: "2026-11-10",
    license: "MIT",
    url: `https://github.com/${repo}`,
    note: "",
  })),
};

const offers = [
  ["claude-bughunter", "security", "Bug bounty e red team externo: 83 skills e 15 comandos (/hunt, /recon, /chain...)."],
  ["cli-anything", "development", "Gera um CLI completo e com estado para qualquer programa de interface gráfica."],
  ["stitch-skills", "design", "Google Stitch: design para código (React, shadcn/ui, Remotion) e sistema de design."],
  ["astryx", "design", "Astryx (Meta), sistema de design e biblioteca de UI pelo comando astryx."],
  ["billion-context", "productivity", "Conversas longas pelo proxy local billion-context."],
].map(([name, category, description]) => ({ market: "zerith", name, category, description, version: "", homepage: "" }));

const plugins = {
  installed: [
    {
      name: "claude-bughunter",
      version: "2.1.0",
      description: offers[0].description,
      capabilities: [] as string[],
      trusted: false,
      enabled: true,
      skills: 98,
      agents: 0,
      runsCode: true,
    },
    {
      name: "astryx",
      version: "0.1.0",
      description: offers[3].description,
      capabilities: [] as string[],
      trusted: false,
      enabled: true,
      skills: 1,
      agents: 0,
      runsCode: false,
    },
  ],
  offers,
  markets: [{ name: "zerith", source: "Mistertelecom/zerith" }],
};

const agents = [
  [
    "explorer",
    "Lê o código e responde perguntas sem editar nada.",
    "claude-haiku-5-5",
    false,
    false,
  ],
  [
    "planner",
    "Quebra uma tarefa grande em passos verificáveis.",
    "claude-opus-5-5",
    false,
    false,
  ],
  ["reviewer", "Revisa diffs como um sênior chato.", null, true, false],
  ["scraper", "Coleta dados da web.", "gpt-5-codex", true, true],
].map(([name, description, model, createdByAi, disabled]) => ({
  name,
  description,
  model,
  instructions: `Você é o agente ${name}.`,
  path: `C:/Users/Joao/AppData/Roaming/Zerith/agents/${name}.md`,
  layer: createdByAi ? "user" : "builtin",
  tools: name === "explorer" ? ["read_file", "glob", "grep"] : null,
  disallowedTools: [],
  permissionMode: null,
  maxTurns: null,
  skills: [],
  isolation: "none",
  background: false,
  createdByAi,
  disabled,
}));

const history = sessions.map((s, i) => ({
  id: s.id,
  title: s.title,
  source: i === 3 ? "hermes" : "native",
  model: s.model,
  provider: s.provider,
  createdAt: s.created_at,
  updatedAt: s.updated_at,
  archived: i === 4,
}));

const mediaModels = {
  total: 3,
  models: [
    ["black-forest-labs/flux-1.1-pro", "FLUX 1.1 Pro", "$0.04 / imagem"],
    ["google/nanobanana", "Nano Banana", "$0.03 / imagem"],
    ["recraft/recraft-v3", "Recraft V3", "$0.04 / imagem"],
  ].map(([id, name, priceHint]) => ({
    id,
    name,
    description: "Geração de imagem de alta fidelidade.",
    priceHint,
    params: [
      {
        name: "aspect_ratio",
        kind: "enum",
        options: ["1:1", "16:9", "9:16"],
        min: null,
        max: null,
      },
      { name: "steps", kind: "int", options: [], min: 1, max: 50 },
    ],
    maxReferences: 1,
    frameImages: [],
  })),
};

const messages: Record<
  string,
  {
    id?: number;
    role: string;
    content: string;
    reasoning: string | null;
    tool_name: string | null;
  }[]
> = {
  s1: [
    {
      role: "user",
      content:
        "Migre o cliente HTTP do provedor OpenAI e mantenha os testes passando.",
      reasoning: null,
      tool_name: null,
    },
    {
      id: 2,
      role: "assistant",
      content:
        "Vou ler o cliente atual, trocar a implementação e rodar os testes.\n\n## O que muda\n\n1. `openai.rs` passa a usar `http::client(timeout)`\n2. A URL é montada com `join(base, path)`\n3. **Nenhuma** mudança na API pública\n\n```rust\nlet client = http::client(timeout);\nlet url = join(base, path);\nsend(client)\n```\n\nRisco: a mudança toca o fluxo de *streaming* — vou rodar `cargo test -p zerith-providers` no fim." +
        "\n\n| Arquivo | Linhas | Estado |\n|:--|--:|:--|\n| `openai.rs` | 214 | editado |\n| `http.rs` | 38 | novo |\n| `codex.rs` | 0 | sem mudança |\n\n![Prévia da tela](" +
        PREVIEW_IMG +
        ")",
      reasoning:
        "O cliente atual cria um agente por chamada; reaproveitar evita handshake TLS repetido.",
      tool_name: null,
    },
  ],
};

const PREVIEW_CHANGES = [
  {
    path: "C:/Users/Joao/Documents/PROJETOS PESSOAIS/zerith/crates/zerith-providers/src/openai.rs",
    rel: "crates/zerith-providers/src/openai.rs",
    status: "alterado",
    added: 4,
    removed: 3,
    diff:
      "@@ -10,8 +10,9 @@\n use crate::http;\n \n pub fn send(base: &str, path: &str) -> Result<String> {\n-    let agent = ureq::agent();\n-    let url = format!(\"{base}{path}\");\n+    let client = http::client(timeout);\n+    let url = join(base, path);\n+    // um cliente só por processo\n-    agent.get(&url).call()?.into_string()\n+    client.get(&url).call()?.into_string()\n }\n \n",
    note: null,
    truncated: false,
    changedSince: false,
  },
  {
    path: "C:/Users/Joao/Documents/PROJETOS PESSOAIS/zerith/crates/zerith-providers/src/http.rs",
    rel: "crates/zerith-providers/src/http.rs",
    status: "criado",
    added: 4,
    removed: 0,
    diff: "@@ -0,0 +1,4 @@\n+//! Cliente HTTP compartilhado.\n+pub fn client(timeout: u64) -> ureq::Agent {\n+    ureq::AgentBuilder::new().timeout_read(timeout).build()\n+}\n",
    note: null,
    truncated: false,
    changedSince: null,
  },
  {
    path: "C:/Users/Joao/Documents/PROJETOS PESSOAIS/zerith/docs/logo.png",
    rel: "docs/logo.png",
    status: "alterado",
    added: 0,
    removed: 0,
    diff: null,
    note: "arquivo binário: sem diff",
    truncated: false,
    changedSince: true,
  },
];

let state = {
  theme: "dark",
  memory_paused: false,
  // `&jobs=1` in the preview URL: one hung dev server to stop.
  jobs: new URLSearchParams(globalThis.location?.search ?? "").has("jobs")
    ? [{ id: "job-3", command: "npm run dev -- --port 5173", session: "s1", secs: 754 }]
    : ([] as { id: string; command: string; session: string | null; secs: number }[]),
};

export function installMock(theme: string) {
  state.theme = theme;
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "plugin:app|version":
          return "0.1.1";
        case "update_check":
          // &update=1 in the preview URL: a newer release is out
          return new URLSearchParams(location.search).get("update")
            ? { current: "0.1.1", latest: "0.1.2", newer: true, notes: "", message: "Nova versão 0.1.2 disponível" }
            : { current: "0.1.1", latest: "0.1.1", newer: false, notes: "", message: "Você está na versão mais nova" };
        case "boot":
          return {
            root: "C:/Users/Joao/AppData/Roaming/Zerith",
            db: "C:/Users/Joao/AppData/Roaming/Zerith/zerith.db",
            theme: state.theme,
            profile: "balanced",
            reduce_motion: false,
            sessions,
            providers,
            memory_paused: state.memory_paused,
          };
        case "list_sessions":
          return sessions;
        case "session_messages":
          return messages[a.id as string] ?? [];
        case "providers":
          return providers;
        case "presets":
          return presets;
        case "accounts":
          return accounts;
        case "account_test":
          return {
            ok: true,
            status: 200,
            latency_ms: 212,
            models: ["llama-3.3-70b-versatile", "qwen-qwq-32b"],
            detail: "",
          };
        case "account_save": {
          // the core upserts by id and answers with the fresh list
          const f = a.form as Record<string, unknown>;
          const list = accounts as unknown as Record<string, unknown>[];
          const i = list.findIndex((x) => x.id === f.id);
          const prev =
            i >= 0
              ? list[i]
              : { ...list[list.length - 1], auth: "api-key", oauth_tool: null };
          const next = {
            ...prev,
            id: f.id as string,
            provider: f.provider as string,
            label: (f.label as string | null) || prev.label,
            base_url: (f.base_url as string | null) || prev.base_url,
            protocol: (f.protocol as string | null) ?? prev.protocol,
            default_model: (f.default_model as string | null) || null,
            priority: f.priority as number,
            enabled: f.enabled as boolean,
            quota_tokens: f.quota_tokens as number | null,
            window_hours: f.window_hours as number,
          };
          if (i >= 0) list[i] = next;
          else list.push(next);
          return [...list];
        }
        case "account_delete":
          accounts.splice(
            accounts.findIndex((x) => x.id === a.id),
            1,
          );
          return [...accounts];
        case "oauth_status":
          return a.account === "claude-assinatura"
            ? { tool: "claude-code", minutes: 47 }
            : a.account === "codex"
              ? { tool: "codex", minutes: 0 }
              : null;
        case "routing":
          return {
            main_model: "claude-assinatura:claude-sonnet-5-5",
            subagent_model: "codex:gpt-5-codex",
            explore_model: "deepseek-trabalho:deepseek-chat",
            plan_model: null,
            role_models: { reviewer: "claude-assinatura:claude-opus-5-5" },
            combos: [
              {
                name: "rapido",
                models: [
                  "claude-assinatura:claude-haiku-5-5",
                  "deepseek-trabalho:deepseek-chat",
                  "codex:gpt-5-codex",
                ],
              },
            ],
          };
        case "usage":
          return usage;
        case "account_quota": {
          const w = (u: number, h: number) => ({
            utilization: u,
            resets_at: new Date(Date.now() + h * 3600e3).toISOString(),
            resets_at_epoch: null,
          });
          if (a.account === "antigravity")
            return {
              email: null,
              plan: "Claude and GPT models",
              five_hour: w(0, 4),
              seven_day: w(0.2, 117),
              buckets: [
                {
                  group: "Claude and GPT models",
                  label: "Semanal",
                  window: w(0.2, 117),
                },
                {
                  group: "Claude and GPT models",
                  label: "Sessão (5 h)",
                  window: w(0, 4),
                },
                ...[
                  "Claude Opus 5.5 (High)",
                  "Claude Sonnet 5.5 (High)",
                  "Gemini 3 Flash",
                  "Gemini 3.1 Flash Image",
                  "GPT-OSS 120B (Medium)",
                ].map((m, i) => ({
                  group: "Modelos",
                  label: m,
                  window: w([12, 74, 0, 35, 93][i], 4),
                })),
              ],
            };
          if (a.account === "codex")
            return {
              email: "voce@exemplo.com",
              plan: "plus",
              five_hour: w(0, 5),
              seven_day: w(100, 7),
              buckets: [
                { group: "Janelas", label: "Sessão (5 h)", window: w(0, 5) },
                { group: "Janelas", label: "Semanal", window: w(100, 7) },
              ],
            };
          return {
            email: "voce@exemplo.com",
            plan: "claude_pro",
            five_hour: w(18, 3),
            seven_day: w(81, 98),
            buckets: [
              { group: "Janelas", label: "Sessão (5 h)", window: w(18, 3) },
              { group: "Janelas", label: "Semanal", window: w(81, 98) },
              { group: "Janelas", label: "Semanal · Opus", window: w(93, 98) },
            ],
          };
        }
        case "memory_paused":
          return state.memory_paused;
        case "memory_facts":
          return facts;
        case "memory_search":
          return [1, 3];
        case "skills_list":
          return allSkills;
        case "skills_remote":
          return {
            repo: a.repo,
            branch: "main",
            fetched_at: now * 1000,
            skills: remoteSkills,
          };
        case "skill_install_remote": {
          const r = remoteSkills.find((x) => x.path === a.path)!;
          allSkills.push(
            skillView(
              r.name,
              r.description,
              `github:${a.repo}/${r.path}`,
              "enabled",
            ),
          );
          return [...allSkills];
        }
        case "skill_toggle":
          allSkills.find((x) => x.installed.name === a.name)!.installed.status =
            a.enabled ? "enabled" : "disabled";
          return [...allSkills];
        case "skills_catalog":
          return catalog;
        case "skills_plugins":
          return plugins;
        case "plugin_action": {
          const o = offers.find((x) => x.name === a.target);
          if (a.action === "install" && o && !plugins.installed.some((p) => p.name === o.name)) {
            plugins.installed.push({
              name: o.name,
              version: "1.0.0",
              description: o.description,
              capabilities: [],
              trusted: false,
              enabled: true,
              skills: 5,
              agents: 0,
              runsCode: false,
            });
          }
          return { ...plugins, installed: [...plugins.installed] };
        }
        case "hermes_skills":
          return [
            ["", "ab-testing", "Plan and run A/B tests and experiments."],
            ["", "brainstorming", "Explore intent before any creative work."],
            ["mlops/inference", "llama-cpp", "llama.cpp local GGUF inference + HF Hub model discovery."],
            ["mlops/inference", "serving-llms-vllm", "Serve LLMs with vLLM."],
            ["networking", "mikrotik-routeros-cli", "Use when accessing a MikroTik RouterOS device via SSH."],
            ["software-development", "zerith-harness", "Use when working in the Zerith Rust agent harness."],
          ].map(([category, name, description]) => ({
            name,
            category,
            description,
            installed: name === "brainstorming",
            error: null,
          }));
        case "hermes_skills_import":
          return { imported: a.names as string[], existed: [], failed: [], skills: [...allSkills] };
        case "mcp_list":
          return [
            { name: "codebase-memory-mcp", transport: "stdio", target: "npx --yes codebase-memory-mcp", enabled: true, approved: true, project: false, envKeys: [], headerKeys: [] },
            { name: "github", transport: "stdio", target: "npx -y @modelcontextprotocol/server-github", enabled: true, approved: true, project: false, envKeys: ["GITHUB_TOKEN"], headerKeys: [] },
            { name: "docs", transport: "http", target: "https://exemplo.test/mcp", enabled: false, approved: true, project: false, envKeys: [], headerKeys: ["Authorization"] },
            { name: "vercel", transport: "http", target: "https://mcp.vercel.com", enabled: true, approved: true, project: false, envKeys: [], headerKeys: [], oauth: true, connected: true },
            { name: "supabase", transport: "http", target: "https://mcp.supabase.com/mcp", enabled: true, approved: true, project: false, envKeys: [], headerKeys: [], oauth: true, connected: false },
          ];
        case "mcp_oauth_login":
          setTimeout(() => void emit("oauth:url", "https://api.supabase.com/v1/oauth/authorize?client_id=exemplo"), 50);
          return new Promise(() => {}); // waits for the browser forever
        case "mcp_oauth_logout":
          return [];
        case "mcp_test":
          return [
            { name: "search_graph", description: "Find symbols" },
            { name: "query_graph", description: "Cypher" },
            { name: "trace_path", description: "Callers/callees" },
          ];
        case "mcp_hermes_preview":
          return [
            { name: "centauri", transport: "stdio", target: "/Users/apple/.local/share/pycentauri/centauri-mcp.sh", status: "nao-encontrado", enabled: false, envKeys: [] },
            { name: "vercel", transport: "http", target: "https://mcp.vercel.com", status: "oauth", enabled: true, envKeys: [] },
            { name: "codebase-memory-mcp", transport: "stdio", target: "npx --yes codebase-memory-mcp", status: "existe", enabled: true, envKeys: [] },
            { name: "rea", transport: "stdio", target: "C:\\Program Files\\nodejs\\node.exe rea.mjs mcp", status: "importavel", enabled: true, envKeys: ["GHIDRA_INSTALL_DIR", "JAVA_HOME"] },
          ];
        case "token_saver":
          return { ponytail: "full", caveman: "off", rtkEnabled: true };
        case "long_context":
        case "set_long_context":
        case "install_billion_context":
          return { enabled: true, state: "running", detail: "http://127.0.0.1:18787", log: "" };
        case "storage":
          return {
            root: "C:/Users/Joao/AppData/Roaming/Zerith",
            dbPath: "C:/Users/Joao/AppData/Roaming/Zerith/zerith.db",
            dbBytes: 18_400_000,
            sessions: 214,
            messages: 9_812,
            backupDir: "C:/Users/Joao/AppData/Roaming/Zerith/backups",
          };
        case "profile_limits":
          return [
            { name: "economy", maxSubagents: 1, historyWindow: 8 },
            { name: "balanced", maxSubagents: 3, historyWindow: 20 },
            { name: "performance", maxSubagents: 6, historyWindow: 60 },
          ];
        case "media_models":
          return mediaModels;
        case "media_key":
          return {
            source: "keyring",
            label: "openrouter",
            usageUsd: 3.42,
            limitUsd: 20,
          };
        case "media_settings":
          return {
            image: "black-forest-labs/flux-1.1-pro",
            imageParams: { aspect_ratio: "16:9" },
            video: "",
            videoParams: {},
            outputDir: "C:/Users/Joao/Pictures/Zerith",
          };
        case "agents_list":
          return agents;
        case "sessions_history":
          return history.filter((h) => a.includeArchived || !h.archived);
        case "hermes_detect":
          return [
            {
              path: "C:/Users/Joao/AppData/Local/hermes",
              profile: "default",
              sessions: [],
              totalMessages: 4_210,
              minDate: now - 90 * 86400,
              maxDate: now - 3600,
            },
          ];
        case "checkpoints":
          return [1, 2];
        case "turn_changes":
          return a.messageId === 2 ? PREVIEW_CHANGES : [];
        case "turn_undo":
          return { done: true, conflicts: [], filesRestored: 1, filesRemoved: 1, errors: [] };
        case "read_image":
          return PREVIEW_IMG;
        case "save_attachment":
          return `C:/Users/Joao/AppData/Roaming/Zerith/attachments/${String(a.data).length}.png`;
        case "recent_workspaces":
          return [
            "C:/Users/Joao/Documents/PROJETOS PESSOAIS/zerith",
            "C:/Users/Joao/Documents/PROJETOS PESSOAIS/site-provedor",
            "C:/Users/Joao/Documents/notas",
          ];
        case "pick_folder":
          return null;
        case "workspace_files":
          return [
            "crates/zerith-providers/src/openai.rs",
            "crates/zerith-providers/src/oauth.rs",
            "README.md",
          ];
        case "set_memory_paused":
          state.memory_paused = a.paused as boolean;
          return null;
        case "save_prefs":
          return state.theme;
        case "background_jobs":
          return state.jobs;
        case "background_job_stop":
          state.jobs = state.jobs.filter((j) => j.id !== a.id);
          return `${a.id} encerrado`;
        default:
          // A list command nobody mocked yet (`terminals_list` blanked the
          // whole preview: `null.some`) answers an empty list, not null.
          return cmd.endsWith("_list") || cmd.endsWith("s_history") ? [] : null;
      }
    },
    { shouldMockEvents: true },
  );
}
