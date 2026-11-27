// Providers Zerith can connect to, generated from 9router's catalogue (MIT,
// see assets/providers/LICENSE-9router.txt) and filtered to what the core can
// actually run: OpenAI-compatible or Anthropic chat endpoints with a key, the
// local servers, and the three OAuth subscriptions. Regenerate rather than
// hand-edit; `models` are 9router's suggestions, not a live list.

export type CatalogAuth = "oauth" | "apikey" | "local";

/** Catalogue entries that are not chat models (embeddings, voice, image). */
export const NOT_CHAT =
  /embed|tts|whisper|dall-e|moderation|rerank|transcribe/i;

export type CatalogEntry = {
  /** Provider name written to settings (and the logo key). */
  id: string;
  name: string;
  auth: CatalogAuth;
  /** The core's provider for this entry (for OAuth, the account's provider). */
  provider: string;
  oauthTool?: "claude-code" | "codex" | "antigravity";
  protocol: "openai" | "anthropic" | "responses";
  baseUrl: string;
  color?: string;
  website?: string;
  keyUrl?: string;
  free?: boolean;
  featured?: boolean;
  models?: { id: string; name: string }[];
};

export const CATALOG: CatalogEntry[] = [
  {
    id: "claude",
    name: "Claude Code",
    auth: "oauth",
    oauthTool: "claude-code",
    provider: "anthropic",
    protocol: "anthropic",
    baseUrl: "https://api.anthropic.com/v1",
    color: "#D97757",
    website: "https://claude.ai",
    featured: true,
    models: [
      {
        id: "claude-fable-5-2",
        name: "Claude Fable 5.2",
      },
      {
        id: "claude-opus-5-5",
        name: "Claude Opus 5.5",
      },
      {
        id: "claude-opus-5",
        name: "Claude Opus 5",
      },
      {
        id: "claude-fable-5",
        name: "Claude Fable 5",
      },
      {
        id: "claude-sonnet-5-5",
        name: "Claude Sonnet 5.5",
      },
      {
        id: "claude-sonnet-5",
        name: "Claude Sonnet 5",
      },
      {
        id: "claude-haiku-5-5",
        name: "Claude Haiku 5.5",
      },
      {
        id: "claude-haiku-4-5-20251001",
        name: "Claude 4.5 Haiku",
      },
    ],
  },
  {
    id: "codex",
    name: "OpenAI Codex",
    auth: "oauth",
    oauthTool: "codex",
    provider: "codex",
    protocol: "responses",
    baseUrl: "https://chatgpt.com/backend-api/codex",
    color: "#3B82F6",
    website: "https://chatgpt.com/codex",
    featured: true,
    models: [
      {
        id: "gpt-6.1-sol",
        name: "GPT 6.1 Sol",
      },
      {
        id: "gpt-6.1-terra",
        name: "GPT 6.1 Terra",
      },
      {
        id: "gpt-6-astra",
        name: "GPT 6.0 Astra",
      },
      {
        id: "gpt-6-astra[1m]",
        name: "GPT 6.0 Astra (extended context)",
      },
      {
        id: "gpt-6-sol",
        name: "GPT 6.0 Sol",
      },
      {
        id: "gpt-6-sol[1m]",
        name: "GPT 6.0 Sol (extended context)",
      },
      {
        id: "gpt-6-luna",
        name: "GPT 6.0 Luna",
      },
      {
        id: "gpt-6-luna[1m]",
        name: "GPT 6.0 Luna (extended context)",
      },
      {
        id: "gpt-5.6-sol",
        name: "GPT 5.6 Sol",
      },
      {
        id: "gpt-5.6-sol[1m]",
        name: "GPT 5.6 Sol (extended context)",
      },
      {
        id: "gpt-5.6-sol-review",
        name: "GPT 5.6 Sol Review",
      },
      {
        id: "gpt-5.6-terra",
        name: "GPT 5.6 Terra",
      },
      {
        id: "gpt-5.6-terra[1m]",
        name: "GPT 5.6 Terra (extended context)",
      },
      {
        id: "gpt-5.6-terra-review",
        name: "GPT 5.6 Terra Review",
      },
      {
        id: "gpt-5.6-luna",
        name: "GPT 5.6 Luna",
      },
      {
        id: "gpt-5.6-luna[1m]",
        name: "GPT 5.6 Luna (extended context)",
      },
      {
        id: "gpt-5.6-luna-review",
        name: "GPT 5.6 Luna Review",
      },
      {
        id: "gpt-daybreak-blue-latest",
        name: "GPT Daybreak Blue",
      },
      {
        id: "gpt-reserve",
        name: "GPT Reserve",
      },
      {
        id: "codex-auto-review",
        name: "Codex Auto Review",
      },
      {
        id: "gpt-image-2.5",
        name: "GPT Image 2.5",
      },
      {
        id: "gpt-image-2.5-flare",
        name: "GPT Image 2.5 Flare",
      },
      {
        id: "gpt-image-2.5-sunburst",
        name: "GPT Image 2.5 Sunburst",
      },
      {
        id: "gpt-image-2.6-flare",
        name: "GPT Image 2.6 Flare",
      },
      {
        id: "gpt-image-2.6-sunburst",
        name: "GPT Image 2.6 Sunburst",
      },
      {
        id: "gpt-image-2",
        name: "GPT Image 2",
      },
      {
        id: "gpt-image-1.5",
        name: "GPT Image 1.5",
      },
      {
        id: "gpt-6-sol-image",
        name: "GPT 6.0 Sol Image",
      },
      {
        id: "transcribe",
        name: "Codex Transcription",
      },
    ],
  },
  {
    id: "antigravity",
    name: "Antigravity",
    auth: "oauth",
    oauthTool: "antigravity",
    provider: "antigravity",
    protocol: "openai",
    baseUrl: "https://daily-cloudcode-pa.googleapis.com",
    color: "#F59E0B",
    website: "https://antigravity.google",
    featured: true,
    models: [
      {
        id: "gemini-4-carbon-high",
        name: "Gemini 4 Carbon (High)",
      },
      {
        id: "gemini-4-carbon-medium",
        name: "Gemini 4 Carbon (Medium)",
      },
      {
        id: "gemini-4-carbon-low",
        name: "Gemini 4 Carbon (Low)",
      },
      {
        id: "gemini-4-carbon",
        name: "Gemini 4 Carbon",
      },
      {
        id: "gemini-4-argon-high",
        name: "Gemini 4 Argon (High)",
      },
      {
        id: "gemini-4-argon-medium",
        name: "Gemini 4 Argon (Medium)",
      },
      {
        id: "gemini-4-argon-low",
        name: "Gemini 4 Argon (Low)",
      },
      {
        id: "gemini-3.8-flash-high",
        name: "Gemini 3.8 Flash (High)",
      },
      {
        id: "gemini-3.8-flash-medium",
        name: "Gemini 3.8 Flash (Medium)",
      },
      {
        id: "gemini-3.8-flash-low",
        name: "Gemini 3.8 Flash (Low)",
      },
      {
        id: "gemini-3.8-flash",
        name: "Gemini 3.8 Flash",
      },
      {
        id: "gemini-3.7-flash-high",
        name: "Gemini 3.7 Flash (High)",
      },
      {
        id: "gemini-3.7-flash-medium",
        name: "Gemini 3.7 Flash (Medium)",
      },
      {
        id: "gemini-3.7-flash-low",
        name: "Gemini 3.7 Flash (Low)",
      },
      {
        id: "gemini-3.6-flash-high",
        name: "Gemini 3.6 Flash (High)",
      },
      {
        id: "gemini-3.6-flash-medium",
        name: "Gemini 3.6 Flash (Medium)",
      },
      {
        id: "gemini-3.6-flash-low",
        name: "Gemini 3.6 Flash (Low)",
      },
      {
        id: "gemini-3.1-pro-high",
        name: "Gemini 3.1 Pro (High)",
      },
      {
        id: "gemini-pro-agent",
        name: "Gemini 3.1 Pro (High)",
      },
      {
        id: "gemini-3.1-pro-low",
        name: "Gemini 3.1 Pro (Low)",
      },
      {
        id: "claude-sonnet-5-5-high",
        name: "Claude Sonnet 5.5 (High)",
      },
      {
        id: "claude-sonnet-5-5-medium",
        name: "Claude Sonnet 5.5 (Medium)",
      },
      {
        id: "claude-sonnet-5-5-low",
        name: "Claude Sonnet 5.5 (Low)",
      },
      {
        id: "claude-sonnet-5-5",
        name: "Claude Sonnet 5.5 (Thinking)",
      },
      {
        id: "claude-opus-5-5-high",
        name: "Claude Opus 5.5 (High)",
      },
      {
        id: "claude-opus-5-5-medium",
        name: "Claude Opus 5.5 (Medium)",
      },
      {
        id: "claude-opus-5-5-low",
        name: "Claude Opus 5.5 (Low)",
      },
      {
        id: "claude-opus-5-5",
        name: "Claude Opus 5.5 (Thinking)",
      },
      {
        id: "claude-sonnet-4-6",
        name: "Claude Sonnet 4.6 (Thinking)",
      },
      {
        id: "claude-opus-4-6-thinking",
        name: "Claude Opus 4.6 (Thinking)",
      },
    ],
  },
  {
    id: "anthropic",
    name: "Claude (API)",
    auth: "apikey",
    provider: "anthropic",
    protocol: "anthropic",
    baseUrl: "https://api.anthropic.com/v1",
    color: "#D97757",
    website: "https://console.anthropic.com",
    keyUrl: "https://console.anthropic.com/settings/keys",
    featured: true,
    models: [
      {
        id: "claude-sonnet-4-20250514",
        name: "Claude Sonnet 4",
      },
      {
        id: "claude-opus-4-20250514",
        name: "Claude Opus 4",
      },
      {
        id: "claude-3-5-sonnet-20241022",
        name: "Claude 3.5 Sonnet",
      },
    ],
  },
  {
    id: "openai",
    name: "OpenAI",
    auth: "apikey",
    provider: "openai",
    protocol: "openai",
    baseUrl: "https://api.openai.com/v1",
    color: "#10A37F",
    website: "https://platform.openai.com",
    keyUrl: "https://platform.openai.com/api-keys",
    featured: true,
    models: [
      {
        id: "gpt-5.5",
        name: "GPT-5.5",
      },
      {
        id: "gpt-5.4",
        name: "GPT-5.4",
      },
      {
        id: "gpt-5.4-mini",
        name: "GPT-5.4 Mini",
      },
      {
        id: "gpt-5.4-nano",
        name: "GPT-5.4 Nano",
      },
      {
        id: "gpt-5.2",
        name: "GPT-5.2",
      },
      {
        id: "gpt-5.1",
        name: "GPT-5.1",
      },
      {
        id: "gpt-5",
        name: "GPT-5",
      },
      {
        id: "gpt-5-mini",
        name: "GPT-5 Mini",
      },
      {
        id: "gpt-5-nano",
        name: "GPT-5 Nano",
      },
      {
        id: "gpt-4o",
        name: "GPT-4o",
      },
      {
        id: "gpt-4o-mini",
        name: "GPT-4o Mini",
      },
      {
        id: "gpt-4-turbo",
        name: "GPT-4 Turbo",
      },
      {
        id: "gpt-4.1",
        name: "GPT-4.1",
      },
      {
        id: "gpt-4.1-mini",
        name: "GPT-4.1 Mini",
      },
      {
        id: "gpt-4.1-nano",
        name: "GPT-4.1 Nano",
      },
      {
        id: "o3",
        name: "O3",
      },
      {
        id: "o3-mini",
        name: "O3 Mini",
      },
      {
        id: "o3-pro",
        name: "O3 Pro",
      },
      {
        id: "o4-mini",
        name: "O4 Mini",
      },
      {
        id: "o1",
        name: "O1",
      },
      {
        id: "o1-mini",
        name: "O1 Mini",
      },
      {
        id: "text-embedding-3-large",
        name: "Text Embedding 3 Large",
      },
      {
        id: "text-embedding-3-small",
        name: "Text Embedding 3 Small",
      },
      {
        id: "text-embedding-ada-002",
        name: "Text Embedding Ada 002",
      },
      {
        id: "tts-1",
        name: "TTS-1",
      },
      {
        id: "tts-1-hd",
        name: "TTS-1 HD",
      },
      {
        id: "gpt-4o-mini-tts",
        name: "GPT-4o Mini TTS",
      },
      {
        id: "whisper-1",
        name: "Whisper 1",
      },
      {
        id: "gpt-4o-transcribe",
        name: "GPT-4o Transcribe",
      },
      {
        id: "gpt-4o-mini-transcribe",
        name: "GPT-4o Mini Transcribe",
      },
    ],
  },
  {
    id: "gemini",
    name: "Gemini",
    auth: "apikey",
    provider: "gemini",
    protocol: "openai",
    baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai",
    color: "#4285F4",
    website: "https://ai.google.dev",
    keyUrl: "https://aistudio.google.com/app/apikey",
    free: true,
    featured: true,
    models: [
      {
        id: "gemini-4-argon",
        name: "Gemini 4 Argon",
      },
      {
        id: "gemini-3.8-flash",
        name: "Gemini 3.8 Flash",
      },
      {
        id: "gemini-3.7-flash",
        name: "Gemini 3.7 Flash",
      },
      {
        id: "gemini-3.6-flash",
        name: "Gemini 3.6 Flash",
      },
      {
        id: "gemini-3.5-flash-lite",
        name: "Gemini 3.5 Flash Lite",
      },
      {
        id: "gemini-3.1-pro-preview",
        name: "Gemini 3.1 Pro Preview",
      },
      {
        id: "gemini-3.1-flash-lite-preview",
        name: "Gemini 3.1 Flash Lite Preview",
      },
      {
        id: "gemini-3-flash-preview",
        name: "Gemini 3 Flash Preview",
      },
      {
        id: "gemini-2.5-pro",
        name: "Gemini 2.5 Pro",
      },
      {
        id: "gemini-2.5-flash",
        name: "Gemini 2.5 Flash",
      },
      {
        id: "gemini-2.5-flash-lite",
        name: "Gemini 2.5 Flash Lite",
      },
      {
        id: "gemma-4-31b-it",
        name: "Gemma 4 31B IT",
      },
      {
        id: "gemini-embedding-2-preview",
        name: "Gemini Embedding 2 Preview",
      },
      {
        id: "gemini-embedding-001",
        name: "Gemini Embedding 001",
      },
      {
        id: "text-embedding-005",
        name: "Text Embedding 005",
      },
      {
        id: "text-embedding-004",
        name: "Text Embedding 004 (Legacy)",
      },
      {
        id: "gemini-3.1-flash-image-preview",
        name: "Gemini 3.1 Flash Image (Nano Banana 2)",
      },
      {
        id: "gemini-3-pro-image-preview",
        name: "Gemini 3 Pro Image (Nano Banana Pro)",
      },
      {
        id: "gemini-2.5-flash-image",
        name: "Gemini 2.5 Flash Image (Nano Banana)",
      },
      {
        id: "gemini-2.5-pro",
        name: "Gemini 2.5 Pro (Best)",
      },
      {
        id: "gemini-2.5-flash",
        name: "Gemini 2.5 Flash",
      },
      {
        id: "gemini-2.5-flash-lite",
        name: "Gemini 2.5 Flash Lite (Cheapest)",
      },
      {
        id: "gemini-2.0-flash",
        name: "Gemini 2.0 Flash",
      },
      {
        id: "gemini-2.5-flash-native-audio-preview-09-17",
        name: "Gemini Live Transcription (Realtime)",
      },
      {
        id: "gemini-3.1-flash-tts-preview",
        name: "Gemini 3.1 Flash TTS",
      },
      {
        id: "gemini-2.5-flash-preview-tts",
        name: "Gemini 2.5 Flash TTS",
      },
      {
        id: "gemini-2.5-pro-preview-tts",
        name: "Gemini 2.5 Pro TTS",
      },
      {
        id: "embedding-001",
        name: "Embedding 001",
      },
    ],
  },
  {
    id: "deepseek",
    name: "DeepSeek",
    auth: "apikey",
    provider: "deepseek",
    protocol: "openai",
    baseUrl: "https://api.deepseek.com/v1",
    color: "#4D6BFE",
    website: "https://deepseek.com",
    keyUrl: "https://platform.deepseek.com/api_keys",
    featured: true,
    models: [
      {
        id: "deepseek-v4-pro",
        name: "DeepSeek V4 Pro",
      },
      {
        id: "deepseek-v4-pro-max",
        name: "DeepSeek V4 Pro Max",
      },
      {
        id: "deepseek-v4-pro-none",
        name: "DeepSeek V4 Pro No Thinking",
      },
      {
        id: "deepseek-v4.1-flash",
        name: "DeepSeek V4.1 Flash",
      },
      {
        id: "deepseek-v4-flash",
        name: "DeepSeek V4 Flash",
      },
      {
        id: "deepseek-v4-flash-vision-exp",
        name: "DeepSeek V4 Flash Vision (Exp)",
      },
      {
        id: "deepseek-chat",
        name: "DeepSeek V3.2 Chat",
      },
      {
        id: "deepseek-reasoner",
        name: "DeepSeek V3.2 Reasoner",
      },
    ],
  },
  {
    id: "openrouter",
    name: "OpenRouter",
    auth: "apikey",
    provider: "openrouter",
    protocol: "openai",
    baseUrl: "https://openrouter.ai/api/v1",
    color: "#F97316",
    website: "https://openrouter.ai",
    keyUrl: "https://openrouter.ai/settings/keys",
    free: true,
    featured: true,
    models: [
      {
        id: "microsoft/microsoft-decision-1.1",
        name: "Microsoft Decision 1.1",
      },
      {
        id: "openai/text-embedding-3-large",
        name: "OpenAI Text Embedding 3 Large",
      },
      {
        id: "openai/text-embedding-3-small",
        name: "OpenAI Text Embedding 3 Small",
      },
      {
        id: "openai/text-embedding-ada-002",
        name: "OpenAI Text Embedding Ada 002",
      },
      {
        id: "qwen/qwen3-embedding-8b",
        name: "Qwen3 Embedding 8B",
      },
      {
        id: "perplexity/pplx-embed-v1-4b",
        name: "Perplexity Embed V1 4B",
      },
      {
        id: "perplexity/pplx-embed-v1-0.6b",
        name: "Perplexity Embed V1 0.6B",
      },
      {
        id: "nvidia/llama-nemotron-embed-vl-1b-v2:free",
        name: "NVIDIA Nemotron Embed VL 1B V2 (Free)",
      },
      {
        id: "openai/gpt-4o-mini-tts",
        name: "GPT-4o Mini TTS",
      },
      {
        id: "openai/tts-1-hd",
        name: "TTS-1 HD",
      },
      {
        id: "openai/tts-1",
        name: "TTS-1",
      },
      {
        id: "openai/dall-e-3",
        name: "DALL-E 3 (via OpenRouter)",
      },
      {
        id: "openai/gpt-image-1",
        name: "GPT Image 1 (via OpenRouter)",
      },
      {
        id: "google/imagen-3.0-generate-002",
        name: "Imagen 3 (via OpenRouter)",
      },
      {
        id: "black-forest-labs/FLUX.1-schnell",
        name: "FLUX.1 Schnell (via OpenRouter)",
      },
      {
        id: "google/veo-3.1",
        name: "Veo 3.1 (via OpenRouter)",
      },
      {
        id: "openai/sora-2-pro",
        name: "Sora 2 Pro (via OpenRouter)",
      },
      {
        id: "bytedance/seedance-2.0",
        name: "Seedance 2.0 (via OpenRouter)",
      },
      {
        id: "typesafe/jev-1.13",
        name: "Jev 1.13",
      },
    ],
  },
  {
    id: "groq",
    name: "Groq",
    auth: "apikey",
    provider: "groq",
    protocol: "openai",
    baseUrl: "https://api.groq.com/openai/v1",
    color: "#F55036",
    website: "https://groq.com",
    keyUrl: "https://console.groq.com/keys",
    free: true,
    featured: true,
    models: [
      {
        id: "llama-3.3-70b-versatile",
        name: "Llama 3.3 70B",
      },
      {
        id: "meta-llama/llama-4-maverick-17b-128e-instruct",
        name: "Llama 4 Maverick",
      },
      {
        id: "qwen/qwen3-32b",
        name: "Qwen3 32B",
      },
      {
        id: "openai/gpt-oss-120b",
        name: "GPT-OSS 120B",
      },
      {
        id: "whisper-large-v3",
        name: "Whisper Large v3",
      },
      {
        id: "whisper-large-v3-turbo",
        name: "Whisper Large v3 Turbo",
      },
      {
        id: "distil-whisper-large-v3-en",
        name: "Distil Whisper Large v3 EN",
      },
    ],
  },
  {
    id: "mistral",
    name: "Mistral",
    auth: "apikey",
    provider: "mistral",
    protocol: "openai",
    baseUrl: "https://api.mistral.ai/v1",
    color: "#FF7000",
    website: "https://mistral.ai",
    keyUrl: "https://console.mistral.ai/api-keys",
    featured: true,
    models: [
      {
        id: "mistral-large-latest",
        name: "Mistral Large 3",
      },
      {
        id: "codestral-latest",
        name: "Codestral",
      },
      {
        id: "mistral-medium-latest",
        name: "Mistral Medium 3",
      },
      {
        id: "mistral-embed",
        name: "Mistral Embed",
      },
    ],
  },
  {
    id: "ollama-local",
    name: "Ollama",
    auth: "local",
    provider: "ollama",
    protocol: "openai",
    baseUrl: "http://localhost:11434/v1",
    color: "#ffffffff",
    website: "https://ollama.com",
    free: true,
    featured: true,
    models: [
      {
        id: "clef",
        name: "Clef",
      },
      {
        id: "clef-flash",
        name: "Clef Flash",
      },
      {
        id: "phonon:latest",
        name: "Phonon (Latest)",
      },
      {
        id: "phonon",
        name: "Phonon",
      },
      {
        id: "phonon:multilingual",
        name: "Phonon Multilingual",
      },
      {
        id: "phonon:40m-multilingual",
        name: "Phonon 40M Multilingual",
      },
      {
        id: "phonon:en",
        name: "Phonon English",
      },
      {
        id: "phonon:31m-en",
        name: "Phonon 31M English",
      },
      {
        id: "phonon:40m-en",
        name: "Phonon 40M English",
      },
      {
        id: "phonon:de",
        name: "Phonon German",
      },
      {
        id: "phonon:40m-de",
        name: "Phonon 40M German",
      },
      {
        id: "phonon:es",
        name: "Phonon Spanish",
      },
      {
        id: "phonon:40m-es",
        name: "Phonon 40M Spanish",
      },
      {
        id: "phonon:fr",
        name: "Phonon French",
      },
      {
        id: "phonon:40m-fr",
        name: "Phonon 40M French",
      },
      {
        id: "phonon:pt",
        name: "Phonon Portuguese",
      },
      {
        id: "phonon:40m-pt",
        name: "Phonon 40M Portuguese",
      },
      {
        id: "nomic-embed-text",
        name: "Nomic Embed Text",
      },
      {
        id: "nomic-embed-text:latest",
        name: "Nomic Embed Text (Latest)",
      },
      {
        id: "nomic-embed-text:v1.5",
        name: "Nomic Embed Text v1.5",
      },
      {
        id: "nomic-embed-text:137m-v1.5-fp16",
        name: "Nomic Embed Text 137M v1.5 (FP16)",
      },
      {
        id: "nomic-embed-text-v2-moe",
        name: "Nomic Embed Text v2 MoE",
      },
      {
        id: "nomic-embed-text-v2-moe:latest",
        name: "Nomic Embed Text v2 MoE (Latest)",
      },
      {
        id: "qwen3-embedding",
        name: "Qwen3 Embedding",
      },
      {
        id: "qwen3-embedding:latest",
        name: "Qwen3 Embedding (Latest)",
      },
      {
        id: "qwen3-embedding:8b",
        name: "Qwen3 Embedding 8B",
      },
      {
        id: "qwen3-embedding:8b-q4_K_M",
        name: "Qwen3 Embedding 8B (Q4_K_M)",
      },
      {
        id: "qwen3-embedding:8b-q8_0",
        name: "Qwen3 Embedding 8B (Q8_0)",
      },
      {
        id: "qwen3-embedding:8b-fp16",
        name: "Qwen3 Embedding 8B (FP16)",
      },
      {
        id: "qwen3-embedding:4b",
        name: "Qwen3 Embedding 4B",
      },
    ],
  },
  {
    id: "lmstudio",
    name: "LM Studio",
    auth: "local",
    provider: "lmstudio",
    protocol: "openai",
    baseUrl: "http://localhost:1234/v1",
    website: "https://lmstudio.ai",
    featured: true,
  },
  {
    id: "agnes",
    name: "Agnes AI",
    auth: "apikey",
    provider: "agnes",
    protocol: "openai",
    baseUrl: "https://apihub.agnes-ai.com/v1",
    color: "#7C3AED",
    website: "https://agnes-ai.com",
    keyUrl: "https://platform.agnes-ai.com",
    free: true,
    models: [
      {
        id: "agnes-2.5-flash",
        name: "Agnes 2.5 Flash",
      },
      {
        id: "agnes-2.5-pro",
        name: "Agnes 2.5 Pro",
      },
      {
        id: "agnes-2.5-pro-beta",
        name: "Agnes 2.5 Pro Beta",
      },
      {
        id: "agnes-3.0-flash",
        name: "Agnes 3.0 Flash",
      },
    ],
  },
  {
    id: "alicode",
    name: "Alibaba",
    auth: "apikey",
    provider: "alicode",
    protocol: "openai",
    baseUrl: "https://coding.dashscope.aliyuncs.com/v1",
    color: "#FF6A00",
    website: "https://bailian.console.aliyun.com",
    keyUrl: "https://bailian.console.aliyun.com/?apiKey=1",
    models: [
      {
        id: "qwen3.5-plus",
        name: "Qwen3.5 Plus",
      },
      {
        id: "kimi-k2.5",
        name: "Kimi K2.5",
      },
      {
        id: "glm-5",
        name: "GLM 5",
      },
      {
        id: "MiniMax-M2.5",
        name: "MiniMax M2.5",
      },
      {
        id: "qwen3-max-2026-01-23",
        name: "Qwen3 Max",
      },
      {
        id: "qwen3-coder-next",
        name: "Qwen3 Coder Next",
      },
      {
        id: "qwen3-coder-plus",
        name: "Qwen3 Coder Plus",
      },
      {
        id: "glm-4.7",
        name: "GLM 4.7",
      },
    ],
  },
  {
    id: "alicode-intl",
    name: "Alibaba Coding",
    auth: "apikey",
    provider: "alicode-intl",
    protocol: "openai",
    baseUrl: "https://coding-intl.dashscope.aliyuncs.com/v1",
    color: "#FF6A00",
    website: "https://www.alibabacloud.com/product/coding",
    keyUrl: "https://www.alibabacloud.com/product/coding",
    models: [
      {
        id: "qwen3.5-plus",
        name: "Qwen3.5 Plus",
      },
      {
        id: "kimi-k2.5",
        name: "Kimi K2.5",
      },
      {
        id: "glm-5",
        name: "GLM 5",
      },
      {
        id: "MiniMax-M2.5",
        name: "MiniMax M2.5",
      },
      {
        id: "qwen3-coder-next",
        name: "Qwen3 Coder Next",
      },
      {
        id: "qwen3-coder-plus",
        name: "Qwen3 Coder Plus",
      },
      {
        id: "glm-4.7",
        name: "GLM 4.7",
      },
    ],
  },
  {
    id: "alims-intl",
    name: "Alibaba Studio",
    auth: "apikey",
    provider: "alims-intl",
    protocol: "openai",
    baseUrl: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
    color: "#FF6A00",
    website: "https://modelstudio.console.alibabacloud.com",
    keyUrl: "https://modelstudio.console.alibabacloud.com/?apiKey=1",
    models: [
      {
        id: "qwen3.5-plus",
        name: "Qwen3.5 Plus",
      },
      {
        id: "kimi-k2.5",
        name: "Kimi K2.5",
      },
      {
        id: "glm-5",
        name: "GLM 5",
      },
      {
        id: "MiniMax-M2.5",
        name: "MiniMax M2.5",
      },
      {
        id: "qwen3-coder-next",
        name: "Qwen3 Coder Next",
      },
      {
        id: "qwen3-coder-plus",
        name: "Qwen3 Coder Plus",
      },
      {
        id: "glm-4.7",
        name: "GLM 4.7",
      },
    ],
  },
  {
    id: "alitp-intl",
    name: "Alibaba Token Plan",
    auth: "apikey",
    provider: "alitp-intl",
    protocol: "openai",
    baseUrl:
      "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
    color: "#FF6A00",
    website: "https://www.alibabacloud.com/campaign/ai-landing-page-token",
    keyUrl: "https://modelstudio.console.alibabacloud.com/?apiKey=1",
    models: [
      {
        id: "qwen3.8-max-preview",
        name: "Qwen3.8 Max Preview",
      },
      {
        id: "qwen3.7-max",
        name: "Qwen3.7 Max",
      },
      {
        id: "qwen3.7-plus",
        name: "Qwen3.7 Plus",
      },
      {
        id: "qwen3.6-flash",
        name: "Qwen3.6 Flash",
      },
      {
        id: "glm-5.2",
        name: "GLM 5.2",
      },
      {
        id: "deepseek-v4-pro",
        name: "DeepSeek V4 Pro",
      },
    ],
  },
  {
    id: "atria",
    name: "Atria Dawn",
    auth: "apikey",
    provider: "atria",
    protocol: "openai",
    baseUrl: "https://api.atria-asi.ai/v1",
    color: "#C2410C",
    website: "https://atria-asi.ai",
    keyUrl: "https://api.atria-asi.ai/dashboard",
    models: [
      {
        id: "Atria-Dawn-Preview",
        name: "Atria Dawn Preview",
      },
    ],
  },
  {
    id: "bai",
    name: "B.AI",
    auth: "apikey",
    provider: "bai",
    protocol: "openai",
    baseUrl: "https://api.b.ai/v1",
    color: "#0369A1",
    website: "https://b.ai",
    keyUrl: "https://b.ai",
  },
  {
    id: "blackbox",
    name: "Blackbox AI",
    auth: "apikey",
    provider: "blackbox",
    protocol: "openai",
    baseUrl: "https://api.blackbox.ai/v1",
    color: "#5B5FEF",
    website: "https://blackbox.ai",
    keyUrl: "https://www.blackbox.ai/api-management",
    models: [
      {
        id: "claude-fable-5",
        name: "Claude Fable 5",
      },
      {
        id: "claude-opus-4.8",
        name: "Claude Opus 4.8",
      },
      {
        id: "claude-sonnet-4.6",
        name: "Claude Sonnet 4.6",
      },
      {
        id: "gpt-5.5",
        name: "GPT-5.5",
      },
      {
        id: "gpt-5.4-pro",
        name: "GPT-5.4 Pro",
      },
      {
        id: "gpt-5.4",
        name: "GPT-5.4",
      },
      {
        id: "gpt-5.3-codex",
        name: "GPT-5.3 Codex",
      },
      {
        id: "gpt-5.4-nano",
        name: "GPT-5.4 Nano",
      },
      {
        id: "deepseek-v4-flash",
        name: "DeepSeek V4 Flash",
      },
      {
        id: "grok-4.3",
        name: "Grok 4.3",
      },
    ],
  },
  {
    id: "byteplus",
    name: "BytePlus ModelArk",
    auth: "apikey",
    provider: "byteplus",
    protocol: "openai",
    baseUrl: "https://ark.ap-southeast.bytepluses.com/api/coding/v3",
    color: "#2563EB",
    website: "https://console.byteplus.com/ark",
    keyUrl: "https://console.byteplus.com/ark/region:ark+ap-southeast-1/apiKey",
    free: true,
    models: [
      {
        id: "seed-2-0-pro-260328",
        name: "Seed 2.0 Pro",
      },
      {
        id: "seed-2-0-code-preview-260328",
        name: "Seed 2.0 Code Preview",
      },
      {
        id: "seed-2-0-mini-260215",
        name: "Seed 2.0 Mini",
      },
      {
        id: "seed-2-0-lite-260228",
        name: "Seed 2.0 Lite",
      },
      {
        id: "kimi-k2-thinking-251104",
        name: "Kimi K2 Thinking",
      },
      {
        id: "glm-4-7-251222",
        name: "GLM 4.7",
      },
      {
        id: "gpt-oss-120b-250805",
        name: "GPT-OSS-120B",
      },
    ],
  },
  {
    id: "cerebras",
    name: "Cerebras",
    auth: "apikey",
    provider: "cerebras",
    protocol: "openai",
    baseUrl: "https://api.cerebras.ai/v1",
    color: "#FF4F00",
    website: "https://www.cerebras.ai",
    keyUrl: "https://cloud.cerebras.ai/platform",
    models: [
      {
        id: "gpt-oss-120b",
        name: "GPT OSS 120B",
      },
      {
        id: "zai-glm-4.7",
        name: "ZAI GLM 4.7",
      },
      {
        id: "llama-3.3-70b",
        name: "Llama 3.3 70B",
      },
      {
        id: "llama-4-scout-17b-16e-instruct",
        name: "Llama 4 Scout",
      },
      {
        id: "qwen-3-235b-a22b-instruct-2507",
        name: "Qwen3 235B A22B",
      },
      {
        id: "qwen-3-32b",
        name: "Qwen3 32B",
      },
    ],
  },
  {
    id: "cohere",
    name: "Cohere",
    auth: "apikey",
    provider: "cohere",
    protocol: "openai",
    baseUrl: "https://api.cohere.ai/compatibility/v1",
    color: "#39594D",
    website: "https://cohere.com",
    keyUrl: "https://dashboard.cohere.com/api-keys",
    models: [
      {
        id: "command-r-plus-08-2024",
        name: "Command R+ (Aug 2024)",
      },
      {
        id: "command-r-08-2024",
        name: "Command R (Aug 2024)",
      },
      {
        id: "command-a-03-2025",
        name: "Command A (Mar 2025)",
      },
    ],
  },
  {
    id: "dahl",
    name: "Dahl Inference",
    auth: "apikey",
    provider: "dahl",
    protocol: "openai",
    baseUrl: "https://inference.dahl.global/v1",
    color: "#1E40AF",
    website: "https://dahl.global",
    keyUrl: "https://dahl.global/dashboard",
    models: [
      {
        id: "zai-org/GLM-5.3-Flash",
        name: "GLM-5.3 Flash",
      },
      {
        id: "deepseek-ai/DeepSeek-V4-Flash-0731",
        name: "DeepSeek V4 Flash 0731",
      },
      {
        id: "MiniMaxAI/MiniMax-M2.7",
        name: "MiniMax M2.7",
      },
    ],
  },
  {
    id: "featherless",
    name: "Featherless",
    auth: "apikey",
    provider: "featherless",
    protocol: "openai",
    baseUrl: "https://api.featherless.ai/v1",
    color: "#111827",
    website: "https://featherless.ai",
    keyUrl: "https://featherless.ai/account/api-keys",
    models: [
      {
        id: "deepseek-ai/DeepSeek-V4-Pro",
        name: "DeepSeek V4 Pro",
      },
      {
        id: "deepseek-ai/DeepSeek-V4-Flash",
        name: "DeepSeek V4 Flash",
      },
      {
        id: "zai-org/GLM-5.2",
        name: "GLM 5.2",
      },
      {
        id: "zai-org/GLM-5.1",
        name: "GLM 5.1",
      },
      {
        id: "moonshotai/Kimi-K2.7-Code",
        name: "Kimi K2.7 Code",
      },
      {
        id: "moonshotai/Kimi-K2.6",
        name: "Kimi K2.6",
      },
      {
        id: "moonshotai/Kimi-K2.5",
        name: "Kimi K2.5",
      },
    ],
  },
  {
    id: "fireworks",
    name: "Fireworks AI",
    auth: "apikey",
    provider: "fireworks",
    protocol: "openai",
    baseUrl: "https://api.fireworks.ai/inference/v1",
    color: "#7B2EF2",
    website: "https://fireworks.ai",
    keyUrl: "https://fireworks.ai/account/api-keys",
    models: [
      {
        id: "accounts/fireworks/models/deepseek-v3p1",
        name: "DeepSeek V3.1",
      },
      {
        id: "accounts/fireworks/models/llama-v3p3-70b-instruct",
        name: "Llama 3.3 70B",
      },
      {
        id: "accounts/fireworks/models/qwen3-235b-a22b",
        name: "Qwen3 235B",
      },
      {
        id: "nomic-ai/nomic-embed-text-v1.5",
        name: "Nomic Embed Text v1.5",
      },
    ],
  },
  {
    id: "glm-cn",
    name: "GLM (China)",
    auth: "apikey",
    provider: "glm-cn",
    protocol: "openai",
    baseUrl: "https://open.bigmodel.cn/api/coding/paas/v4",
    color: "#DC2626",
    website: "https://open.bigmodel.cn",
    keyUrl: "https://open.bigmodel.cn/usercenter/apikeys",
    models: [
      {
        id: "glm-5.3",
        name: "GLM 5.3",
      },
      {
        id: "glm-5.3-flash",
        name: "GLM 5.3 Flash (Vision)",
      },
      {
        id: "glm-5.2",
        name: "GLM 5.2",
      },
      {
        id: "glm-5.1",
        name: "GLM 5.1",
      },
      {
        id: "glm-5-turbo",
        name: "GLM 5 Turbo",
      },
      {
        id: "glm-5",
        name: "GLM 5",
      },
      {
        id: "glm-4.7",
        name: "GLM-4.7",
      },
      {
        id: "glm-4.6v",
        name: "GLM 4.6V (Vision)",
      },
      {
        id: "glm-4.6",
        name: "GLM-4.6",
      },
      {
        id: "glm-4.5-air",
        name: "GLM-4.5-Air",
      },
    ],
  },
  {
    id: "hyperbolic",
    name: "Hyperbolic",
    auth: "apikey",
    provider: "hyperbolic",
    protocol: "openai",
    baseUrl: "https://api.hyperbolic.xyz/v1",
    color: "#00D4FF",
    website: "https://hyperbolic.xyz",
    keyUrl: "https://app.hyperbolic.xyz/settings",
    models: [
      {
        id: "Qwen/QwQ-32B",
        name: "QwQ 32B",
      },
      {
        id: "deepseek-ai/DeepSeek-R1",
        name: "DeepSeek R1",
      },
      {
        id: "deepseek-ai/DeepSeek-V3",
        name: "DeepSeek V3",
      },
      {
        id: "meta-llama/Llama-3.3-70B-Instruct",
        name: "Llama 3.3 70B",
      },
      {
        id: "meta-llama/Llama-3.2-3B-Instruct",
        name: "Llama 3.2 3B",
      },
      {
        id: "Qwen/Qwen2.5-72B-Instruct",
        name: "Qwen 2.5 72B",
      },
      {
        id: "Qwen/Qwen2.5-Coder-32B-Instruct",
        name: "Qwen 2.5 Coder 32B",
      },
      {
        id: "NousResearch/Hermes-3-Llama-3.1-70B",
        name: "Hermes 3 70B",
      },
    ],
  },
  {
    id: "kimchi",
    name: "Kimchi",
    auth: "apikey",
    provider: "kimchi",
    protocol: "openai",
    baseUrl: "https://llm.kimchi.dev/openai/v1",
    color: "#FF521D",
    website: "https://kimchi.dev",
    free: true,
    models: [
      {
        id: "minimax-m3",
        name: "MiniMax-M3",
      },
      {
        id: "kimi-k2.7",
        name: "Kimi-K2.7",
      },
      {
        id: "kimi-k2.6",
        name: "Kimi-K2.6",
      },
      {
        id: "kimi-k2.5",
        name: "Kimi-K2.5",
      },
      {
        id: "nemotron-3-ultra-fp4",
        name: "Nemotron 3 Ultra FP4",
      },
      {
        id: "minimax-m2.7",
        name: "MiniMax-M2.7",
      },
      {
        id: "claude-opus-4-6",
        name: "Claude Opus 4.6",
      },
      {
        id: "claude-sonnet-4-6",
        name: "Claude Sonnet 4.6",
      },
    ],
  },
  {
    id: "minimax-cn",
    name: "Minimax (China)",
    auth: "apikey",
    provider: "minimax-cn",
    protocol: "anthropic",
    baseUrl: "https://api.minimaxi.com/anthropic/v1",
    color: "#DC2626",
    website: "https://www.minimaxi.com",
    keyUrl:
      "https://platform.minimaxi.com/user-center/basic-information/interface-key",
    models: [
      {
        id: "MiniMax-M3",
        name: "MiniMax M3",
      },
      {
        id: "MiniMax-M2.7",
        name: "MiniMax M2.7",
      },
      {
        id: "MiniMax-M2.5",
        name: "MiniMax M2.5",
      },
      {
        id: "MiniMax-M2.1",
        name: "MiniMax M2.1",
      },
      {
        id: "speech-2.8-hd",
        name: "Speech 2.8 HD",
      },
      {
        id: "speech-2.8-turbo",
        name: "Speech 2.8 Turbo",
      },
      {
        id: "speech-2.6-hd",
        name: "Speech 2.6 HD",
      },
      {
        id: "speech-2.6-turbo",
        name: "Speech 2.6 Turbo",
      },
      {
        id: "speech-02-hd",
        name: "Speech 02 HD",
      },
      {
        id: "speech-02-turbo",
        name: "Speech 02 Turbo",
      },
      {
        id: "speech-01-hd",
        name: "Speech 01 HD",
      },
      {
        id: "speech-01-turbo",
        name: "Speech 01 Turbo",
      },
    ],
  },
  {
    id: "minimax",
    name: "Minimax Coding",
    auth: "apikey",
    provider: "minimax",
    protocol: "anthropic",
    baseUrl: "https://api.minimax.io/anthropic/v1",
    color: "#7C3AED",
    website: "https://www.minimaxi.com",
    keyUrl:
      "https://platform.minimaxi.com/user-center/basic-information/interface-key",
    models: [
      {
        id: "MiniMax-M3",
        name: "MiniMax M3",
      },
      {
        id: "MiniMax-M2.7",
        name: "MiniMax M2.7",
      },
      {
        id: "MiniMax-M2.5",
        name: "MiniMax M2.5",
      },
      {
        id: "MiniMax-M2.1",
        name: "MiniMax M2.1",
      },
      {
        id: "minimax-image-01",
        name: "MiniMax Image 01",
      },
      {
        id: "speech-2.8-hd",
        name: "Speech 2.8 HD",
      },
      {
        id: "speech-2.8-turbo",
        name: "Speech 2.8 Turbo",
      },
      {
        id: "speech-2.6-hd",
        name: "Speech 2.6 HD",
      },
      {
        id: "speech-2.6-turbo",
        name: "Speech 2.6 Turbo",
      },
      {
        id: "speech-02-hd",
        name: "Speech 02 HD",
      },
      {
        id: "speech-02-turbo",
        name: "Speech 02 Turbo",
      },
      {
        id: "speech-01-hd",
        name: "Speech 01 HD",
      },
      {
        id: "speech-01-turbo",
        name: "Speech 01 Turbo",
      },
    ],
  },
  {
    id: "nebius",
    name: "Nebius AI",
    auth: "apikey",
    provider: "nebius",
    protocol: "openai",
    baseUrl: "https://api.studio.nebius.ai/v1",
    color: "#6C5CE7",
    website: "https://nebius.com",
    keyUrl: "https://studio.nebius.com/settings/api-keys",
    models: [
      {
        id: "meta-llama/Llama-3.3-70B-Instruct",
        name: "Llama 3.3 70B Instruct",
      },
      {
        id: "Qwen/Qwen3-Embedding-8B",
        name: "Qwen3 Embedding 8B",
      },
    ],
  },
  {
    id: "nvidia",
    name: "NVIDIA NIM",
    auth: "apikey",
    provider: "nvidia",
    protocol: "openai",
    baseUrl: "https://integrate.api.nvidia.com/v1",
    color: "#76B900",
    website: "https://developer.nvidia.com/nim",
    keyUrl: "https://build.nvidia.com/settings/api-keys",
    free: true,
    models: [
      {
        id: "nvidia/nemotron-3.5-lightning-30b-a3b",
        name: "Nemotron 3.5 Lightning",
      },
      {
        id: "nvidia/nemotron-3.5-bolt-e4b",
        name: "Nemotron 3.5 Bolt",
      },
      {
        id: "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning",
        name: "Nemotron 3 Nano Omni",
      },
      {
        id: "nvidia/nemotron-3-ultra-550b-a55b",
        name: "Nemotron 3 Ultra",
      },
      {
        id: "poolside/laguna-xs-2.1",
        name: "Laguna XS 2.1",
      },
      {
        id: "meta/llama-guard-4-12b",
        name: "Llama Guard 4 12B",
      },
      {
        id: "nvidia/riva-translate-4b-instruct-v2",
        name: "Riva Translate 4B v2",
      },
      {
        id: "meta/llama-3.3-70b-instruct",
        name: "Llama 3.3 70B Instruct",
      },
      {
        id: "minimaxai/minimax-m2.7",
        name: "MiniMax M2.7",
      },
      {
        id: "minimaxai/minimax-m3",
        name: "MiniMax M3",
      },
      {
        id: "z-ai/glm-5.2",
        name: "GLM 5.2",
      },
      {
        id: "deepseek-ai/deepseek-v4-pro",
        name: "DeepSeek V4 Pro",
      },
      {
        id: "deepseek-ai/deepseek-v4-flash",
        name: "DeepSeek V4 Flash",
      },
      {
        id: "moonshotai/kimi-k2.6",
        name: "Kimi K2.6",
      },
      {
        id: "nvidia/nv-embedqa-e5-v5",
        name: "NV EmbedQA E5 v5",
      },
      {
        id: "nvidia/parakeet-ctc-1.1b-asr",
        name: "Parakeet CTC 1.1B",
      },
      {
        id: "fastpitch",
        name: "FastPitch",
      },
      {
        id: "tacotron2",
        name: "Tacotron2",
      },
    ],
  },
  {
    id: "ollama-cloud",
    name: "Ollama Cloud",
    auth: "apikey",
    provider: "ollama-cloud",
    protocol: "openai",
    baseUrl: "https://ollama.com/v1",
    color: "#ffffffff",
    website: "https://ollama.com",
    keyUrl: "https://ollama.com/settings/keys",
    free: true,
    models: [
      {
        id: "gpt-oss:120b",
        name: "GPT OSS 120B",
      },
      {
        id: "kimi-k2.5",
        name: "Kimi K2.5",
      },
      {
        id: "glm-5",
        name: "GLM 5",
      },
      {
        id: "minimax-m2.5",
        name: "MiniMax M2.5",
      },
      {
        id: "glm-4.7-flash",
        name: "GLM 4.7 Flash",
      },
      {
        id: "qwen3.5",
        name: "Qwen3.5",
      },
      {
        id: "minimax-m3",
        name: "MiniMax M3",
      },
      {
        id: "deepseek-v4.1-flash:cloud",
        name: "DeepSeek V4.1 Flash",
      },
      {
        id: "clef",
        name: "Clef",
      },
      {
        id: "clef-flash",
        name: "Clef Flash",
      },
      {
        id: "clef:cloud",
        name: "Clef (Cloud)",
      },
    ],
  },
  {
    id: "opencode-go",
    name: "OpenCode Go",
    auth: "apikey",
    provider: "opencode-go",
    protocol: "openai",
    baseUrl: "https://opencode.ai/inference/go/openai/v1",
    color: "#E87040",
    website: "https://opencode.ai/auth",
    keyUrl: "https://opencode.ai/auth",
    models: [
      {
        id: "deepseek-flash",
        name: "DeepSeek Flash",
      },
      {
        id: "glm-5.3-flash",
        name: "GLM 5.3 Flash (Vision)",
      },
      {
        id: "glm-5.3",
        name: "GLM 5.3",
      },
      {
        id: "glm-5.2",
        name: "GLM 5.2",
      },
      {
        id: "glm-5.1",
        name: "GLM 5.1",
      },
      {
        id: "glm-5",
        name: "GLM 5",
      },
      {
        id: "kimi-k2.7-code",
        name: "Kimi K2.7 Code",
      },
      {
        id: "kimi-k2.6",
        name: "Kimi K2.6",
      },
      {
        id: "kimi-k2.5",
        name: "Kimi K2.5",
      },
      {
        id: "kimi-k3",
        name: "Kimi K3",
      },
      {
        id: "kimi-k3.1-pro",
        name: "Kimi K3.1 Pro",
      },
      {
        id: "deepseek-v4-pro",
        name: "DeepSeek V4 Pro",
      },
      {
        id: "deepseek-v4-flash",
        name: "DeepSeek V4 Flash",
      },
      {
        id: "deepseek-v4-flash-vision-exp",
        name: "DeepSeek V4 Flash Vision (Exp)",
      },
      {
        id: "deepseek-v4.1-flash",
        name: "DeepSeek V4.1 Flash",
      },
      {
        id: "longcat-2.0",
        name: "LongCat 2.0",
      },
      {
        id: "mimo-v2.6-flash",
        name: "MiMo V2.6 Flash",
      },
      {
        id: "mimo-v2.6-pro",
        name: "MiMo V2.6 Pro",
      },
      {
        id: "mimo-v2.5",
        name: "MiMo V2.5",
      },
      {
        id: "mimo-v2.5-pro",
        name: "MiMo V2.5 Pro",
      },
      {
        id: "mimo-v2-pro",
        name: "MiMo V2 Pro",
      },
      {
        id: "mimo-v2-omni",
        name: "MiMo V2 Omni",
      },
      {
        id: "minimax-m3",
        name: "MiniMax M3",
      },
      {
        id: "minimax-m2.7",
        name: "MiniMax M2.7",
      },
      {
        id: "minimax-m2.5",
        name: "MiniMax M2.5",
      },
      {
        id: "space-bunny-free",
        name: "Space Bunny Free",
      },
      {
        id: "qwen3.8-max",
        name: "Qwen 3.8 Max",
      },
      {
        id: "qwen3.8-flash",
        name: "Qwen 3.8 Flash",
      },
      {
        id: "qwen3.7-max",
        name: "Qwen 3.7 Max",
      },
      {
        id: "qwen3.7-plus",
        name: "Qwen 3.7 Plus",
      },
    ],
  },
  {
    id: "opencode-zen",
    name: "OpenCode Zen",
    auth: "apikey",
    provider: "opencode-zen",
    protocol: "openai",
    baseUrl: "https://opencode.ai/zen/v1",
    color: "#E87040",
    website: "https://opencode.ai/auth",
    keyUrl: "https://opencode.ai/auth",
    models: [
      {
        id: "claude-fable-5",
        name: "Claude Fable 5",
      },
      {
        id: "claude-opus-5",
        name: "Claude Opus 5",
      },
      {
        id: "claude-opus-4-8",
        name: "Claude Opus 4.8",
      },
      {
        id: "claude-opus-4-7",
        name: "Claude Opus 4.7",
      },
      {
        id: "claude-opus-4-6",
        name: "Claude Opus 4.6",
      },
      {
        id: "claude-opus-4-5",
        name: "Claude Opus 4.5",
      },
      {
        id: "claude-sonnet-5",
        name: "Claude Sonnet 5",
      },
      {
        id: "claude-sonnet-4-6",
        name: "Claude Sonnet 4.6",
      },
      {
        id: "claude-sonnet-4-5",
        name: "Claude Sonnet 4.5",
      },
      {
        id: "claude-sonnet-4",
        name: "Claude Sonnet 4",
      },
      {
        id: "claude-haiku-4-5",
        name: "Claude Haiku 4.5",
      },
      {
        id: "gemini-3.6-flash",
        name: "Gemini 3.6 Flash",
      },
      {
        id: "gemini-3.8-flash",
        name: "Gemini 3.8 Flash",
      },
      {
        id: "gemini-3.7-flash",
        name: "Gemini 3.7 Flash",
      },
      {
        id: "gemini-3.5-flash-lite",
        name: "Gemini 3.5 Flash Lite",
      },
      {
        id: "gemini-3.5-flash",
        name: "Gemini 3.5 Flash",
      },
      {
        id: "gemini-3.1-pro",
        name: "Gemini 3.1 Pro",
      },
      {
        id: "gemini-3-flash",
        name: "Gemini 3 Flash",
      },
      {
        id: "gpt-6-astra",
        name: "GPT 6 Astra",
      },
      {
        id: "gpt-5.6-sol",
        name: "GPT 5.6 Sol",
      },
      {
        id: "gpt-5.6-terra",
        name: "GPT 5.6 Terra",
      },
      {
        id: "gpt-5.6-luna",
        name: "GPT 5.6 Luna",
      },
      {
        id: "gpt-5.5",
        name: "GPT 5.5",
      },
      {
        id: "gpt-5.5-pro",
        name: "GPT 5.5 Pro",
      },
      {
        id: "gpt-5.4",
        name: "GPT 5.4",
      },
      {
        id: "gpt-5.4-pro",
        name: "GPT 5.4 Pro",
      },
      {
        id: "gpt-5.4-mini",
        name: "GPT 5.4 Mini",
      },
      {
        id: "gpt-5.4-nano",
        name: "GPT 5.4 Nano",
      },
      {
        id: "gpt-5.3-codex-spark",
        name: "GPT 5.3 Codex Spark",
      },
      {
        id: "gpt-5.3-codex",
        name: "GPT 5.3 Codex",
      },
    ],
  },
  {
    id: "perplexity",
    name: "Perplexity",
    auth: "apikey",
    provider: "perplexity",
    protocol: "openai",
    baseUrl: "https://api.perplexity.ai",
    color: "#20808D",
    website: "https://www.perplexity.ai",
    keyUrl: "https://www.perplexity.ai/settings/api",
    models: [
      {
        id: "sonar-pro",
        name: "Sonar Pro",
      },
      {
        id: "sonar",
        name: "Sonar",
      },
    ],
  },
  {
    id: "poolside",
    name: "Poolside",
    auth: "apikey",
    provider: "poolside",
    protocol: "openai",
    baseUrl: "https://inference.poolside.ai/v1",
    color: "#0EA5E9",
    website: "https://poolside.ai",
    keyUrl: "https://platform.poolside.ai/api-keys",
    free: true,
    models: [
      {
        id: "poolside/laguna-s-2.1",
        name: "Laguna S 2.1",
      },
      {
        id: "poolside/laguna-xs-2.1",
        name: "Laguna XS 2.1",
      },
    ],
  },
  {
    id: "runcrate",
    name: "Runcrate",
    auth: "apikey",
    provider: "runcrate",
    protocol: "openai",
    baseUrl: "https://api.runcrate.ai/v1",
    color: "#1A2B79",
    website:
      "https://www.runcrate.ai/inference/free?utm_source=9router&utm_medium=directory&utm_campaign=gpu_pricing",
    keyUrl: "https://www.runcrate.ai/dashboard/api-keys",
    models: [
      {
        id: "deepseek-ai/DeepSeek-V4.1-Flash",
        name: "DeepSeek V4.1 Flash",
      },
      {
        id: "deepseek-ai/DeepSeek-V4.1-Flash-1017",
        name: "DeepSeek V4.1 Flash 1017",
      },
      {
        id: "zai-org/GLM-5.3",
        name: "GLM 5.3",
      },
      {
        id: "zai-org/GLM-5.3-Flash",
        name: "GLM 5.3 Flash",
      },
      {
        id: "Qwen/Qwen3.8-Flash",
        name: "Qwen3.8 Flash",
      },
      {
        id: "openai/gpt-oss-120b",
        name: "gpt-oss 120B",
      },
    ],
  },
  {
    id: "siliconflow",
    name: "SiliconFlow",
    auth: "apikey",
    provider: "siliconflow",
    protocol: "openai",
    baseUrl: "https://api.siliconflow.com/v1",
    color: "#5B6EF5",
    website: "https://cloud.siliconflow.com",
    keyUrl: "https://cloud.siliconflow.com/account/ak",
    models: [
      {
        id: "deepseek-ai/DeepSeek-V4-Pro",
        name: "DeepSeek V4 Pro",
      },
      {
        id: "deepseek-ai/DeepSeek-V4-Flash",
        name: "DeepSeek V4 Flash",
      },
      {
        id: "deepseek-ai/DeepSeek-V3.2",
        name: "DeepSeek V3.2",
      },
      {
        id: "deepseek-ai/DeepSeek-V3.2-Exp",
        name: "DeepSeek V3.2 Exp",
      },
      {
        id: "deepseek-ai/DeepSeek-V3.1",
        name: "DeepSeek V3.1",
      },
      {
        id: "deepseek-ai/DeepSeek-V3.1-Terminus",
        name: "DeepSeek V3.1 Terminus",
      },
      {
        id: "deepseek-ai/DeepSeek-R1",
        name: "DeepSeek R1",
      },
      {
        id: "Qwen/Qwen3.5-397B-A17B",
        name: "Qwen 3.5 397B A17B",
      },
      {
        id: "Qwen/Qwen3.5-122B-A10B",
        name: "Qwen 3.5 122B A10B",
      },
      {
        id: "zai-org/GLM-5.1",
        name: "GLM 5.1",
      },
      {
        id: "zai-org/GLM-5",
        name: "GLM 5",
      },
      {
        id: "moonshotai/Kimi-K2.6",
        name: "Kimi K2.6",
      },
      {
        id: "moonshotai/Kimi-K2.5",
        name: "Kimi K2.5",
      },
      {
        id: "openai/gpt-oss-120b",
        name: "GPT OSS 120B",
      },
      {
        id: "MiniMaxAI/MiniMax-M2.5",
        name: "MiniMax M2.5",
      },
      {
        id: "inclusionAI/Ling-flash-2.0",
        name: "Ling Flash 2.0",
      },
    ],
  },
  {
    id: "together",
    name: "Together AI",
    auth: "apikey",
    provider: "together",
    protocol: "openai",
    baseUrl: "https://api.together.xyz/v1",
    color: "#0F6FFF",
    website: "https://www.together.ai",
    keyUrl: "https://api.together.xyz/settings/api-keys",
    models: [
      {
        id: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        name: "Llama 3.3 70B Turbo",
      },
      {
        id: "deepseek-ai/DeepSeek-R1",
        name: "DeepSeek R1",
      },
      {
        id: "Qwen/Qwen3-235B-A22B",
        name: "Qwen3 235B",
      },
      {
        id: "meta-llama/Llama-4-Maverick-17B-128E-Instruct-FP8",
        name: "Llama 4 Maverick",
      },
      {
        id: "BAAI/bge-large-en-v1.5",
        name: "BGE Large EN v1.5",
      },
      {
        id: "togethercomputer/m2-bert-80M-8k-retrieval",
        name: "M2 BERT 80M 8K",
      },
    ],
  },
  {
    id: "tokenharbor",
    name: "Token Harbor",
    auth: "apikey",
    provider: "tokenharbor",
    protocol: "openai",
    baseUrl: "https://tokenharbor.ai/v1",
    color: "#0F766E",
    website: "https://tokenharbor.ai",
    keyUrl: "https://tokenharbor.ai/dashboard",
    models: [
      {
        id: "claude-opus-5.5",
        name: "Claude Opus 5.5",
      },
      {
        id: "claude-sonnet-5",
        name: "Claude Sonnet 5",
      },
      {
        id: "gpt-6-astra",
        name: "GPT-6 Astra",
      },
      {
        id: "gpt-6-sol",
        name: "GPT-6 Sol",
      },
      {
        id: "deepseek-v4.1-flash:free",
        name: "DeepSeek V4.1 Flash (Free)",
      },
      {
        id: "grok-4.7",
        name: "Grok 4.7",
      },
    ],
  },
  {
    id: "venice",
    name: "Venice AI",
    auth: "apikey",
    provider: "venice",
    protocol: "openai",
    baseUrl: "https://api.venice.ai/api/v1",
    color: "#DC2626",
    website: "https://venice.ai",
    keyUrl: "https://venice.ai/settings/api",
    models: [
      {
        id: "venice-uncensored-1-2",
        name: "Venice Uncensored 1.2",
      },
      {
        id: "zai-org-glm-5",
        name: "GLM-5",
      },
      {
        id: "qwen3-235b-a22b-instruct-2507",
        name: "Qwen3 235B A22B Instruct",
      },
      {
        id: "qwen3-coder-480b-a35b-instruct-turbo",
        name: "Qwen3 Coder 480B A35B Turbo",
      },
      {
        id: "qwen3-vl-235b-a22b",
        name: "Qwen3 VL 235B A22B",
      },
      {
        id: "deepseek-v4-pro",
        name: "DeepSeek V4 Pro",
      },
      {
        id: "llama-3.3-70b",
        name: "Llama 3.3 70B",
      },
      {
        id: "hermes-3-llama-3.1-405b",
        name: "Hermes 3 Llama 3.1 405B",
      },
      {
        id: "mistral-small-3-2-24b-instruct",
        name: "Mistral Small 3.2 24B",
      },
      {
        id: "text-embedding-3-large",
        name: "Text Embedding 3 Large",
      },
      {
        id: "text-embedding-bge-m3",
        name: "BGE-M3 Embedding",
      },
      {
        id: "text-embedding-qwen3-8b",
        name: "Qwen3 8B Embedding",
      },
      {
        id: "venice-sd35",
        name: "Venice SD3.5",
      },
      {
        id: "flux-2-pro",
        name: "FLUX.2 Pro",
      },
      {
        id: "gpt-image-2",
        name: "GPT Image 2 (via Venice)",
      },
    ],
  },
  {
    id: "vercel-ai-gateway",
    name: "Vercel AI Gateway",
    auth: "apikey",
    provider: "vercel-ai-gateway",
    protocol: "openai",
    baseUrl: "https://ai-gateway.vercel.sh/v1",
    color: "#111827",
    website: "https://vercel.com/ai-gateway",
    keyUrl: "https://vercel.com/dashboard/~/ai-gateway",
  },
  {
    id: "volcengine-ark",
    name: "Volcengine Ark",
    auth: "apikey",
    provider: "volcengine-ark",
    protocol: "openai",
    baseUrl: "https://ark.cn-beijing.volces.com/api/coding/v3",
    color: "#1677FF",
    website: "https://ark.cn-beijing.volces.com",
    keyUrl: "https://console.volcengine.com/ark/region:ark+cn-beijing/apiKey",
    models: [
      {
        id: "Doubao-Seed-2.0-Code",
        name: "Doubao-Seed-2.0-Code",
      },
      {
        id: "Doubao-Seed-2.0-pro",
        name: "Doubao-Seed-2.0-pro",
      },
      {
        id: "Doubao-Seed-2.0-lite",
        name: "Doubao-Seed-2.0-lite",
      },
      {
        id: "Doubao-Seed-Code",
        name: "Doubao-Seed-Code",
      },
      {
        id: "DeepSeek-V4-Flash",
        name: "DeepSeek-V4-Flash",
      },
      {
        id: "DeepSeek-V4-Pro",
        name: "DeepSeek-V4-Pro",
      },
      {
        id: "GLM-5.1",
        name: "GLM-5.1",
      },
      {
        id: "MiniMax-M2.7",
        name: "MiniMax-M2.7",
      },
      {
        id: "Kimi-K2.6",
        name: "Kimi-K2.6",
      },
    ],
  },
  {
    id: "xiaomi-tokenplan",
    name: "Xiaomi MiMo (Token Plan)",
    auth: "apikey",
    provider: "xiaomi-tokenplan",
    protocol: "openai",
    baseUrl: "https://token-plan-sgp.xiaomimimo.com/v1",
    color: "#FF6700",
    website: "https://mimo.xiaomi.com",
    keyUrl: "https://mimo.xiaomi.com",
    models: [
      {
        id: "mimo-v2.6-pro",
        name: "MiMo V2.6 Pro",
      },
      {
        id: "mimo-v2.6-flash",
        name: "MiMo V2.6 Flash",
      },
      {
        id: "mimo-v2.5-asr",
        name: "MiMo V2.5 ASR",
      },
      {
        id: "mimo-v2.5-tts",
        name: "MiMo V2.5 TTS",
      },
      {
        id: "mimo-v2.5-tts-voiceclone",
        name: "MiMo V2.5 TTS Voice Clone",
      },
      {
        id: "mimo-v2.5-tts-voicedesign",
        name: "MiMo V2.5 TTS Voice Design",
      },
    ],
  },
];

/** Catalogue entry for a provider name, OAuth tool or account provider. */
export function catalogEntry(
  id: string | null | undefined,
): CatalogEntry | undefined {
  if (!id) return undefined;
  const k = id.toLowerCase();
  return (
    CATALOG.find((e) => e.id === k || e.oauthTool === k) ??
    CATALOG.find((e) => e.provider === k)
  );
}
