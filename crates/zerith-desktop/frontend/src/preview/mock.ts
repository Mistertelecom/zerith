// Dev-only: fake Rust core so every screen can be rendered and photographed in
// a plain browser (`/preview.html?scene=...`). Never imported by index.html.
import { mockIPC } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import type { ParamView } from "../lib/api";

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
  ["claude-assinatura", "anthropic", "oauth", "claude-code", 0, true, null, []],
  ["codex", "codex", "oauth", "codex", 1, true, null, []],
  ["antigravity", "antigravity", "oauth", "antigravity", 1, true, null, []],
  ["deepseek-trabalho", "deepseek", "api-key", null, 2, true, 2000000, []],
  [
    "openrouter-backup",
    "openrouter",
    "api-key",
    null,
    5,
    true,
    null,
    [
      "anthropic/claude-sonnet-4.6",
      "microsoft/microsoft-decision-1.1",
      "openai/gpt-5.4",
    ],
  ],
].map(
  ([
    id,
    provider,
    auth,
    oauth_tool,
    priority,
    enabled,
    quota,
    enabled_models,
  ]) => ({
    id,
    provider,
    auth,
    oauth_tool,
    label: null,
    protocol: null,
    base_url: null,
    default_model:
      id === "openrouter-backup" ? "anthropic/claude-sonnet-4.6" : null,
    enabled_models,
    api_key_ref: null,
    priority,
    enabled,
    quota_tokens: quota,
    quota_requests: null,
    window_hours: 5,
  }),
);

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

// Media catalogue: entries and price lines as the core produced them from the
// live OpenRouter lists on 2026-11-24, newest first.
const enumP = (name: string, options: (string | number)[]): ParamView => ({
  name,
  kind: "enum",
  options: options.map(String),
  min: null,
  max: null,
});
const rangeP = (name: string, min: number, max: number): ParamView => ({ name, kind: "range", options: [], min, max });
const flagP = (name: string): ParamView => ({ name, kind: "flag", options: [], min: null, max: null });
const AR = ["1:1", "4:3", "3:4", "16:9", "9:16", "3:2", "2:3"];
const NOW = 1795489510;
const day = 86_400;
type MockModel = [string, string, string, number, ParamView[], number, string[]];
const mediaModel = ([id, name, priceHint, created, params, maxReferences, frameImages]: MockModel) => ({
  id,
  name,
  description: `${name}: modelo de geração no OpenRouter.`,
  priceHint,
  created,
  isNew: NOW - created < 30 * day,
  params,
  maxReferences,
  frameImages,
});
const imageModels: MockModel[] = [
  ["microsoft/mai-image-2.7-flash", "Microsoft AI: MAI-Image-2.7 Flash", "US$ 10,00 por 1M tokens de imagem gerada", NOW - 6 * day, [enumP("aspect_ratio", [...AR, "auto"]), enumP("background", ["opaque", "transparent"]), rangeP("n", 1, 1)], 5, []],
  ["recraft/recraft-v4.1-flash-v2", "Recraft: Recraft V4.1 Flash v2", "US$ 0,007/imagem", NOW - 10 * day, [enumP("aspect_ratio", AR), rangeP("n", 1, 6)], 0, []],
  ["bytedance-seed/seedream-5-5", "ByteDance Seed: Seedream 5.5", "US$ 0,02/imagem · US$ 0,035/imagem (high resolution)", NOW - 11 * day, [enumP("aspect_ratio", AR), enumP("resolution", ["1K", "2K", "4K"]), rangeP("n", 1, 4)], 10, []],
  ["openai/gpt-image-2.6-flare", "OpenAI: GPT Image 2.6 Flare", "US$ 30,00 por 1M tokens de imagem gerada", NOW - 19 * day, [enumP("aspect_ratio", AR), enumP("quality", ["low", "medium", "high"]), enumP("background", ["opaque", "transparent"])], 16, []],
  ["qwen/qwen-image-2.1-pro", "Qwen: Qwen Image 2.1 Pro", "US$ 0,046/imagem (1K / 2K)", NOW - 38 * day, [enumP("aspect_ratio", AR), enumP("resolution", ["1K", "2K"])], 3, []],
  ["black-forest-labs/flux-3-image", "Black Forest Labs: FLUX.3 Image", "US$ 0,041/imagem (768) · US$ 0,048/imagem (1K) · US$ 0,07/imagem (1.5K) · +2", NOW - 53 * day, [enumP("aspect_ratio", AR), enumP("resolution", ["768", "1K", "1.5K", "2K", "4K"])], 8, []],
  ["google/gemini-nano-banana-2.1", "Google: Nano Banana 2.1", "US$ 30,00 por 1M tokens de imagem gerada", NOW - 48 * day, [enumP("aspect_ratio", AR), enumP("resolution", ["1K", "2K", "4K"])], 14, []],
  ["black-forest-labs/flux.2-pro", "Black Forest Labs: FLUX.2 Pro", "US$ 0,03/megapixel (~US$ 0,0315 a 1024×1024)", NOW - 365 * day, [enumP("aspect_ratio", AR), flagP("seed")], 8, []],
];
const videoModels: MockModel[] = [
  ["heygen/heygen-avatar-6-turbo", "HeyGen: Avatar 6 Turbo", "US$ 0,05/s (1080p) · US$ 0,0875/s (2K)", NOW - 6 * day, [enumP("resolution", ["1080p", "2K"])], 0, []],
  ["bytedance/seedance-2.5-flash", "ByteDance: Seedance 2.5 Flash", "US$ 7,00 por 1M tokens de vídeo", NOW - 7 * day, [enumP("duration", [4, 5, 6, 8, 10, 12, 15, 20, 30]), enumP("resolution", ["480p", "720p", "1080p"]), enumP("aspect_ratio", ["16:9", "9:16", "1:1"]), flagP("generate_audio")], 0, ["first_frame", "last_frame"]],
  ["google/gemini-omni-1.2-flash", "Google: Gemini Omni 1.2 Flash", "US$ 0,0232/s (com áudio, 360p) · US$ 0,0695/s (com áudio, 720p) · US$ 0,1043/s (com áudio) · +1", NOW - 18 * day, [enumP("duration", [3, 5, 8, 10, 15]), enumP("resolution", ["360p", "720p", "1080p", "4K"]), flagP("generate_audio")], 0, ["first_frame"]],
  ["x-ai/grok-imagine-video-1.5-lite", "xAI: Grok Imagine Video 1.5 Lite", "US$ 0,02/s (480p) · US$ 0,03/s (720p) · US$ 0,14/s (1080p)", NOW - 48 * day, [enumP("duration", [1, 5, 10, 15]), enumP("resolution", ["480p", "720p", "1080p"]), enumP("aspect_ratio", ["16:9", "9:16"])], 0, ["first_frame"]],
  ["runway/aleph-2", "Runway: Aleph 2", "US$ 0,28/s · mínimo US$ 0,56 por vídeo", NOW - 116 * day, [enumP("aspect_ratio", ["16:9", "9:16"])], 0, []],
  ["kwaivgi/kling-v3.0-pro", "Kling: Kling v3.0 Pro", "US$ 0,112/s · US$ 0,168/s (com áudio)", NOW - 208 * day, [enumP("duration", [3, 5, 10, 15]), enumP("aspect_ratio", ["16:9", "9:16", "1:1"]), flagP("generate_audio")], 0, ["first_frame", "last_frame"]],
  ["google/veo-3.1", "Google: Veo 3.1", "US$ 0,20/s (sem áudio) · US$ 0,40/s (com áudio / sem áudio, 4K) · US$ 0,60/s (com áudio, 4K)", NOW - 245 * day, [enumP("duration", [4, 6, 8]), enumP("resolution", ["720p", "1080p", "4K"]), enumP("aspect_ratio", ["16:9", "9:16"]), flagP("generate_audio")], 0, ["first_frame", "last_frame"]],
];
const mediaModels = (kind: unknown) => {
  const models = (kind === "video" ? videoModels : imageModels).map(mediaModel);
  return { total: models.length, models };
};
const mediaSlot = {
  image: "bytedance-seed/seedream-5-5",
  imageParams: {} as Record<string, unknown>,
  video: "google/veo-3.1",
  videoParams: {} as Record<string, unknown>,
  outputDir: "C:/Users/Joao/Pictures/Zerith",
};

const messages: Record<
  string,
  {
    id?: number;
    role: string;
    content: string;
    reasoning: string | null;
    tool_name: string | null;
    tool_calls?: unknown;
  }[]
> = {
  s1: [
    {"role": "user", "content": "Migre o cliente HTTP do provedor OpenAI e mantenha os testes passando.\n\nContexto:\n- hoje `openai.rs` cria um `ureq::Agent` a cada chamada\n- o *streaming* abre outra conexão por cima\n- o Codex usa o mesmo caminho\n\nRegras:\n1. **nada** muda na API pública\n2. um cliente só por processo\n3. timeout configurável\n\nDepois rode `cargo test -p zerith-providers` e me diga o que falhou.\nSe o teste de streaming quebrar, pare e pergunte antes de mexer no parser.\nNão toque no `codex.rs` nesta rodada.", "reasoning": null, "tool_name": null},
    {"id": 2, "role": "assistant", "content": "", "reasoning": "Primeiro leio `openai.rs` e mapeio os usos de `ureq::Agent`; depois rodo os testes e aplico a edição.", "tool_calls": [{"id": "h0a", "name": "read_file", "arguments": "{\"path\": \"crates/zerith-providers/src/openai.rs\", \"offset\": 1, \"limit\": 80}"}, {"id": "h0b", "name": "grep", "arguments": "{\"pattern\": \"ureq::agent|AgentBuilder\", \"path\": \"crates/zerith-providers/src\"}"}], "tool_name": null},
    {"id": 21, "role": "tool", "tool_name": "read_file", "tool_calls": {"tool_call_id": "h0a"}, "content": "[linhas 1-80 de 214]\n1\tuse crate::http;\n2\tpub struct Client {\n3\t    cfg: ProviderConfig,\n4\t    agent: ureq::Agent,\n5\t}", "reasoning": null},
    {"id": 22, "role": "tool", "tool_name": "grep", "tool_calls": {"tool_call_id": "h0b"}, "content": "crates/zerith-providers/src/openai.rs:41: let agent = ureq::agent();\ncrates/zerith-providers/src/http.rs:12: ureq::AgentBuilder::new()", "reasoning": null},
    {"id": 3, "role": "assistant", "content": "", "reasoning": "Primeiro rodo os testes para ver o **estado atual**; depois edito `openai.rs`.\n\n- se `auth` faltar, o erro aparece no build\n- o `join` já existe em `http.rs`", "tool_calls": [{"id": "h1", "name": "shell", "arguments": "{\"command\": \"cargo test -p zerith-providers --lib 2>&1 | tail -n 5\"}"}], "tool_name": null},
    {"id": 4, "role": "tool", "tool_name": "shell", "tool_calls": {"tool_call_id": "h1"}, "content": "erro: exit 101\n[o comando FALHOU]\nerror[E0063]: missing field `auth` in initializer of `Client`\n  --> crates/zerith-providers/src/openai.rs:41:9\n   |\n41 |         Client { cfg, agent }\n   |         ^^^^^^ missing `auth`\nerror: could not compile `zerith-providers` (lib) due to 1 previous error", "reasoning": null},
    {"id": 5, "role": "assistant", "content": "", "tool_calls": [{"id": "h2", "name": "edit_file", "arguments": "{\"path\": \"crates/zerith-providers/src/openai.rs\", \"old_string\": \"    let agent = ureq::agent();\\n    let url = format!(\\\"{base}{path}\\\");\", \"new_string\": \"    let client = http::client(timeout);\\n    let url = join(base, path);\\n    // um cliente s\\u00f3 por processo\"}"}, {"id": "h3", "name": "net_broadcast", "arguments": "{\"hosts\": [\"edge-sp-01\", \"edge-rj-02\"], \"command\": \"show bgp summary\"}"}], "reasoning": null, "tool_name": null},
    {"id": 6, "role": "tool", "tool_name": "edit_file", "tool_calls": {"tool_call_id": "h2"}, "content": "1 ocorrência(s) substituída(s) em crates/zerith-providers/src/openai.rs", "reasoning": null},
    {"id": 7, "role": "tool", "tool_name": "net_broadcast", "tool_calls": {"tool_call_id": "h3"}, "content": "2 hosts consultados em paralelo\n### edge-sp-01 (18 ms)\n```\nNeighbor 10.254.0.1 AS 65001 State/PfxRcd 412\n```\n### edge-rj-02 (24 ms)\n```\nNeighbor 10.254.0.2 AS 65002 State/PfxRcd 389\n```", "reasoning": null},
    {
      id: 8,
      role: "assistant",
      content:
        "# Cliente HTTP do OpenAI\n\nVou ler o cliente atual, trocar a implementação e rodar os testes. A referência é a [documentação do ureq](https://docs.rs/ureq).\n\n> O teste de streaming fica de fora até você confirmar a troca do parser.\n\n## O que muda\n\n### Arquivos tocados\n\n- `openai.rs`: o cliente\n- `http.rs`: novo, com `client()` e `join()`\n\n### Passos\n\n1. `openai.rs` passa a usar `http::client(timeout)`\n2. A URL é montada com `join(base, path)`\n3. **Nenhuma** mudança na API pública\n\n```rust\nlet client = http::client(timeout);\nlet url = join(base, path);\nsend(client)\n```\n\nRisco: a mudança toca o fluxo de *streaming* — vou rodar `cargo test -p zerith-providers` no fim." +
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

const wfMock = {
  channels: [
    {
      id: "ch-evo",
      name: "WhatsApp empresa",
      kind: "evolution_api",
      enabled: true,
      config: {
        base_url: "http://10.8.0.1:8080",
        instance: "empresa",
        api_key: "••••",
        default_target: "120363025246125486@g.us",
      },
      updated_at_ms: NOW * 1000,
    },
    {
      id: "ch-tg",
      name: "Telegram NOC",
      kind: "telegram",
      enabled: true,
      config: { bot_token: "••••", default_target: "-1001234" },
      updated_at_ms: NOW * 1000,
    },
  ],
  workflows: [
    {
      id: "wf-backup",
      name: "Backup diário dos roteadores",
      description: "",
      icon: "workflow",
      enabled: true,
      nodes: [
        { id: "cron", kind: "trigger_cron", label: "Todo dia 02:00", x: 40, y: 80, config: { cron: "0 2 * * *" } },
        { id: "ssh", kind: "ssh_command", label: "Exportar config", x: 320, y: 80, config: {} },
        { id: "zap", kind: "notify_channel", label: "Avisar grupo", x: 600, y: 80, config: { channel_id: "ch-evo" } },
      ],
      edges: [
        { id: "e1", from: "cron", to: "ssh" },
        { id: "e2", from: "ssh", to: "zap" },
      ],
      created_at_ms: NOW * 1000,
      updated_at_ms: NOW * 1000,
      last_status: "error",
      alert_after_failures: 2,
      alert_channels: ["ch-evo"],
      alert_target: { "ch-evo": "120363025246125486@g.us" },
    },
  ],
};
// Servidores: "vps" online (daemon recente, um workflow falhando) e "casa" fora do ar.
const nowMs = now * 1000;
const servers = [
  { id: "srv-vps", name: "vps", url: "http://198.18.251.1:7878", created_at_ms: nowMs - 9 * 86400e3, last_contact_ms: nowMs - 20e3 },
  { id: "srv-casa", name: "casa", url: "http://100.101.102.103:7878", created_at_ms: nowMs - 30 * 86400e3, last_contact_ms: nowMs - 5 * 3600e3 },
];
const serverStatus = [
  {
    id: "srv-vps",
    color: "green",
    version: "0.4.7",
    health: { name: "zerith", version: "0.4.7", hostname: "vps", uptime_secs: 86400, workflows_total: 2, workflows_enabled: 2, failing_workflows: 1, runs_active: 0 },
    daemon: {
      version: "0.4.7",
      started_at_ms: nowMs - 86400e3,
      last_cycle_ms: nowMs - 25e3,
      hostname: "vps",
      active_workflows: 2,
      next_run: { workflow_id: "wf-backup", at_ms: nowMs + 42 * 60e3 },
      consecutive_failures: { "wf-olt": 3 },
    },
    error: null,
    checked_at_ms: nowMs,
    last_contact_ms: nowMs - 20e3,
  },
  { id: "srv-casa", color: "red", version: null, health: null, daemon: null, error: "sem conexão com o servidor: connection refused", checked_at_ms: nowMs, last_contact_ms: nowMs - 5 * 3600e3 },
];
const wfNode = (id: string, kind: string, label: string, x: number, config: Record<string, unknown>) => ({ id, kind, label, x, y: 140, config });
const wfBase = { description: "", icon: "workflow", edges: [{ id: "e1", from: "n1", to: "n2", branch: null }], created_at_ms: nowMs - 86400e3, updated_at_ms: nowMs - 3600e3, bot_name: null };
const workflows = [
  {
    ...wfBase,
    id: "wf-olt",
    name: "Checar OLT Huawei",
    icon: "radar",
    enabled: true,
    run_on: "srv-vps",
    last_run_at_ms: nowMs - 5 * 60e3,
    last_status: "error",
    nodes: [
      wfNode("n1", "trigger_cron", "A cada 5 min", 60, { cron: "*/5 * * * *" }),
      wfNode("n2", "ssh_command", "display ont info", 350, { host: "10.0.0.2", user: "noc", command: "display ont info 0 all", secret_ref: "olt-borda" }),
    ],
  },
  {
    ...wfBase,
    id: "wf-backup",
    name: "Backup do banco",
    icon: "rocket",
    enabled: true,
    run_on: "srv-vps",
    last_run_at_ms: nowMs - 23 * 3600e3,
    last_status: "ok",
    nodes: [wfNode("n1", "trigger_cron", "Todo dia 03:00", 60, { cron: "0 3 * * *" }), wfNode("n2", "shell", "pg_dump", 350, { command: "pg_dump app > /var/backups/app.sql" })],
  },
  {
    ...wfBase,
    id: "wf-local",
    name: "Resumo da manhã",
    enabled: false,
    run_on: null,
    last_run_at_ms: null,
    last_status: null,
    nodes: [wfNode("n1", "trigger_cron", "Todo dia 08:00", 60, { cron: "0 8 * * *" }), wfNode("n2", "notify_channel", "Avisar", 350, {})],
  },
];
const remoteRuns = [
  {
    id: "run-3",
    workflow_id: "wf-olt",
    trigger_source: "cron",
    status: "error",
    started_at_ms: nowMs - 5 * 60e3,
    finished_at_ms: nowMs - 5 * 60e3 + 10_400,
    summary: "ssh: connect to host 10.0.0.2 port 22: Connection timed out",
    node_logs: [
      { node_id: "n1", label: "A cada 5 min", kind: "trigger_cron", status: "ok", output: "", duration_ms: 0 },
      { node_id: "n2", label: "display ont info", kind: "ssh_command", status: "error", output: "ssh: connect to host 10.0.0.2 port 22: Connection timed out", duration_ms: 10_400 },
    ],
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

let chatSceneEnriched = false;

export function installMock(theme: string) {
  state.theme = theme;
  chatSceneEnriched = false;
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
        case "session_messages": {
          if (
            a.id === "s1" &&
            !chatSceneEnriched &&
            new URLSearchParams(globalThis.location?.search ?? "").get("scene") === "chat"
          ) {
            chatSceneEnriched = true;
            setTimeout(() => {
              void emit("turn:event", [
                "s1",
                {
                  type: "tasks",
                  items: [
                    ["Ler openai.rs e mapear os usos de ureq", "completed"],
                    ["Extrair pool HTTP compartilhado em http.rs", "completed"],
                    ["Validar compatibilidade da API pública", "completed"],
                    ["Migrar chamadas de streaming mantendo timeout", "in_progress"],
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
                },
              ]);
              void emit("turn:event", [
                "s1",
                {
                  type: "subagent",
                  id: "sa2",
                  name: "reviewer: auditar contratos de timeout e retry",
                  state: "done",
                  summary: "API pública preservada; 3 testes de timeout verificados sem regressão.",
                  model: "codex:gpt-5-codex",
                },
              ]);
            }, 420);
          }
          return messages[a.id as string] ?? [];
        }
        case "providers":
          return providers;
        case "presets":
          return presets;
        case "accounts":
          return accounts;
        case "account_test": {
          const f = (a.form as Record<string, unknown>) ?? {};
          const prov = String(f.provider ?? "");
          return {
            ok: true,
            status: 200,
            latency_ms: 184,
            models:
              prov === "openrouter"
                ? [
                    "openai/gpt-5.4",
                    "openai/gpt-5.4-mini",
                    "anthropic/claude-sonnet-4.6",
                    "anthropic/claude-opus-4.6",
                    "microsoft/microsoft-decision-1.1",
                    "google/gemini-3.1-pro",
                    "deepseek/deepseek-v3.2",
                    "qwen/qwen3-coder-480b",
                    "meta-llama/llama-4-maverick:free",
                    "openai/text-embedding-3-large",
                  ]
                : ["llama-3.3-70b-versatile", "qwen-qwq-32b"],
            detail: "",
          };
        }
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
            enabled_models: Array.isArray(f.enabled_models)
              ? (f.enabled_models as string[])
              : (prev.enabled_models ?? []),
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
        case "usage_cost":
          // Conversa: só a assinatura (equivalente na API); período: as três
          // contas, com um modelo local sem preço.
          return a.sessionId
            ? {
                billed: 0,
                subscription: 0.4182,
                priced: true,
                unpriced: [],
                by_model: [
                  { key: "claude-sonnet-5-5", billed: 0, subscription: 0.4182, priced: true },
                ],
                by_day: [{ key: "2026-11-24", billed: 0, subscription: 0.4182, priced: true }],
              }
            : {
                billed: 0.0391,
                subscription: 3.912,
                priced: true,
                unpriced: ["qwen3-coder:30b"],
                by_model: [
                  { key: "claude-sonnet-5-5", billed: 0, subscription: 2.474, priced: true },
                  { key: "gpt-5-codex", billed: 0, subscription: 1.231, priced: true },
                  { key: "claude-haiku-5-5", billed: 0, subscription: 0.207, priced: true },
                  { key: "deepseek-chat", billed: 0.0391, subscription: 0, priced: true },
                  { key: "qwen3-coder:30b", billed: 0, subscription: 0, priced: false },
                ],
                by_day: [
                  { key: "2026-11-22", billed: 0.0102, subscription: 0.884, priced: true },
                  { key: "2026-11-23", billed: 0.0177, subscription: 1.612, priced: true },
                  { key: "2026-11-24", billed: 0.0112, subscription: 1.416, priced: true },
                ],
              };
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
          const targetName = String(a.target ?? "").split("@")[0];
          const o = offers.find((x) => x.name === targetName);
          if (a.action === "install" && o && !plugins.installed.some((p) => p.name === o.name)) {
            plugins.installed.push({
              name: o.name,
              version: o.version || "1.0.0",
              description: o.description,
              capabilities: [],
              trusted: false,
              enabled: true,
              skills: 5,
              agents: 0,
              runsCode: false,
            });
          } else if (a.action === "update" && o) {
            const cur = plugins.installed.find((p) => p.name === o.name);
            if (cur && o.version) cur.version = o.version;
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
          return mediaModels(a.kind);
        case "media_save_slot": {
          const e = a.edit as { kind: string; model: string; params: Record<string, unknown> };
          if (e.kind === "video") Object.assign(mediaSlot, { video: e.model, videoParams: e.params });
          else Object.assign(mediaSlot, { image: e.model, imageParams: e.params });
          return null;
        }
        case "media_key":
          return {
            source: "keyring",
            label: "openrouter",
            usageUsd: 3.42,
            limitUsd: 20,
          };
        case "media_settings":
          return { ...mediaSlot };
        case "agents_list":
          return agents;
        case "sessions_history":
          return history.filter((h) => a.includeArchived || !h.archived);
        case "migration_offer":
          return false;
        case "migration_plan":
        case "migration_apply": {
          const applied = cmd === "migration_apply";
          const st = (s: string) => (applied && s === "imported" ? "imported" : s);
          return {
            origin: "hermes",
            source_root: "C:/Users/Joao/AppData/Local/hermes",
            dry_run: !applied,
            items: [
              { kind: "sessao", source: "state.db", dest: "zerith.db (perfil default)", status: st("imported"), reason: "412 conversas novas (9 fixadas, 380 arquivadas), 0 já importadas" },
              { kind: "memoria", source: "default/memories/MEMORY.md#1", dest: "memory_facts (entity)", status: "imported", reason: "" },
              { kind: "memoria", source: "default/memories/MEMORY.md#2", dest: "memory_facts (entity)", status: "imported", reason: "valor(es) no cofre: hermes-memoria-1a2b3c4d" },
              { kind: "memoria", source: "default/memories/USER.md#1", dest: "memory_facts (instruction)", status: "skipped", reason: "já na memória" },
              { kind: "persona", source: "SOUL.md", dest: "AGENTS.md", status: "skipped", reason: "é o texto padrão que vem com o Hermes" },
              { kind: "persona", source: "AGENTS.md", dest: "archive/persona/AGENTS.md", status: "archived", reason: "guia genérico do pacote Everything Claude Code (ECC)" },
              { kind: "credencial", source: "creds/acme-borda.txt", dest: "inventory.toml (acme/borda)", status: "imported", reason: "senha(s) fora: re-adicione com zerith secret set acme-borda" },
              { kind: "skill", source: "networking/olt-parks", dest: "skills/olt-parks", status: "imported", reason: "criada no Hermes" },
              { kind: "skill", source: "creative/ascii-video", dest: "skills/ascii-video", status: "skipped", reason: "embutida no Hermes e sem alteração" },
              { kind: "mcp", source: "config.yaml: github", dest: "mcp.toml (github)", status: "imported", reason: "env.GITHUB_TOKEN deixado(s) de fora" },
              { kind: "cron", source: "default/cron/jobs.json: resumo", dest: "agendamento hermes-default-resumo", status: "imported", reason: "cron `0 12 * * *` (UTC); entra desligado" },
              { kind: "telegram", source: ".env", dest: "archive/telegram/LEIA.md", status: "archived", reason: "sem equivalente: TELEGRAM_HOME_CHANNEL" },
            ],
            stripped_secrets: ["credencial:acme-borda", "mcp:github.env.GITHUB_TOKEN"],
            backup_dir: applied ? "C:/Users/Joao/AppData/Roaming/Zerith/backups/pre-migracao-20261124-120000" : null,
            report_dir: applied ? "C:/Users/Joao/AppData/Roaming/Zerith/migrations/20261124-120000" : null,
          };
        }
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
          return a.messageId === 8 ? PREVIEW_CHANGES : [];
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
        case "memory_curate_plan":
          return {
            merges: [
              {
                keep: 1,
                text: "O servidor de borda usa BGP com a AS 65001",
                drop: [[3, "servidor de borda usa BGP, AS 65001"]],
              },
            ],
            archives: [{ id: 2, text: "o roteador antigo era um 7200", idle_days: 45 }],
          };
        case "memory_curate_apply":
          return { merged: 1, archived: 1 };
        case "btw_ask":
          return {
            text: "No NX-OS: `route-map NOME permit 10` e depois `match ip address prefix-list LISTA`.",
            model: a.model as string,
            prompt_tokens: 1200,
            completion_tokens: 40,
            cache_read_tokens: 1100,
          };
        case "subagent_stop":
          return true;
        case "servers_list":
          return servers;
        case "servers_status":
          return serverStatus;
        case "server_health":
        case "server_test":
          return serverStatus.find((s) => s.id === a.id);
        case "workflow_list":
          return [...workflows, ...wfMock.workflows];
        case "server_workflows":
          return workflows.filter((w) => w.run_on === a.serverId).map((w) => ({ ...w, run_on: undefined }));
        case "server_workflow_runs":
          return remoteRuns.filter((r) => r.workflow_id === a.id);
        case "workflow_runs":
          return [];
        case "server_secret_names":
          return ["telegram-bot"];
        case "workflow_webhook_base_url":
          return "http://127.0.0.1:47891/webhook/";
        case "background_job_stop":
          state.jobs = state.jobs.filter((j) => j.id !== a.id);
          return `${a.id} encerrado`;
        // Automações: um workflow com alerta e dois canais (cena `channels`).
        case "workflow_save":
          return a.workflow;
        case "channel_list":
          return wfMock.channels;
        case "channel_save":
          return a.channel;
        case "channel_check":
          return "Evolution API: instância 'empresa' conectada (open)";
        case "channel_test":
          return "WhatsApp enviado via Evolution API (empresa) → 120363025246125486@g.us";
        case "channel_groups":
          return [
            { id: "120363025246125486@g.us", name: "Alertas TI", size: 12 },
            { id: "120363111111111111@g.us", name: "Diretoria", size: 5 },
          ];
        default:
          // A list command nobody mocked yet (`terminals_list` blanked the
          // whole preview: `null.some`) answers an empty list, not null.
          return cmd.endsWith("_list") || cmd.endsWith("s_history") ? [] : null;
      }
    },
    { shouldMockEvents: true },
  );
}
