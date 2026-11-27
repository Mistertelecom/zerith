import {
  mergeModelChoices,
  normalizeModelList,
  resolveAccountModelIds,
} from "./accountModels.ts";

function assertEq<T>(actual: T, expected: T, label: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    throw new Error(`${label}\n  actual:   ${a}\n  expected: ${e}`);
  }
}

function assertTrue(cond: boolean, label: string): void {
  if (!cond) throw new Error(`Assertion failed: ${label}`);
}

// 1. normalizeModelList trims, drops empties, and deduplicates preserving order
assertEq(
  normalizeModelList([" a ", "", "b", "a", "  ", "c"]),
  ["a", "b", "c"],
  "normalizeModelList",
);

// 2. When enabled_models is non-empty, it is an exact WYSIWYG whitelist
//    (does not dump catalogue models and keeps explicitly chosen models even if unusual)
const curated = resolveAccountModelIds({
  provider: "openrouter",
  oauth_tool: null,
  default_model: "anthropic/claude-sonnet-4.5",
  enabled_models: [
    "microsoft/microsoft-decision-1.1",
    "openai/gpt-5.4-preview",
    "custom/my-rerank-model",
  ],
});
assertEq(
  curated,
  [
    "microsoft/microsoft-decision-1.1",
    "openai/gpt-5.4-preview",
    "custom/my-rerank-model",
  ],
  "curated whitelist",
);

// 3. When enabled_models is empty, falls back to default_model + catalogue (filtered by !NOT_CHAT)
const fallback = resolveAccountModelIds({
  provider: "openrouter",
  oauth_tool: null,
  default_model: "openai/gpt-5.4",
  enabled_models: [],
});
assertEq(fallback[0], "openai/gpt-5.4", "fallback default first");
assertTrue(fallback.length > 3, "should include openrouter catalogue models");
assertTrue(
  fallback.every((m) => !/embed|tts|whisper/i.test(m)),
  "should exclude non-chat models by default",
);

// 4. OAuth live models still take precedence when enabled_models is empty,
//    but enabled_models overrides them if the user explicitly curated a subset
const oauthLive = resolveAccountModelIds(
  {
    provider: "antigravity",
    oauth_tool: "antigravity",
    default_model: "retired-model",
    enabled_models: [],
  },
  { live: [{ id: "gemini-4-pro" }, { id: "text-embedding-004" }] },
);
assertEq(oauthLive, ["gemini-4-pro"], "oauth live");

const oauthCurated = resolveAccountModelIds(
  {
    provider: "antigravity",
    oauth_tool: "antigravity",
    default_model: null,
    enabled_models: ["gemini-4-flash"],
  },
  { live: [{ id: "gemini-4-pro" }, { id: "gemini-4-flash" }] },
);
assertEq(oauthCurated, ["gemini-4-flash"], "oauth curated");

// 5. mergeModelChoices preserves API order first, enriches names from catalogue,
//    and keeps selected / default models even if absent from API & catalogue
const merged = mergeModelChoices({
  liveIds: ["openai/gpt-5.4", "anthropic/claude-sonnet-4.5", "openai/gpt-5.4"],
  catalogModels: [
    { id: "anthropic/claude-sonnet-4.5", name: "Claude Sonnet 4.5" },
    { id: "deepseek/deepseek-chat", name: "DeepSeek Chat" },
  ],
  selectedIds: ["custom/private-model"],
  defaultModel: "custom/default-only",
});
assertEq(
  merged,
  [
    { id: "openai/gpt-5.4", name: "openai/gpt-5.4", fromApi: true },
    {
      id: "anthropic/claude-sonnet-4.5",
      name: "Claude Sonnet 4.5",
      fromApi: true,
    },
    { id: "deepseek/deepseek-chat", name: "DeepSeek Chat", fromApi: false },
    {
      id: "custom/private-model",
      name: "custom/private-model",
      fromApi: false,
    },
    { id: "custom/default-only", name: "custom/default-only", fromApi: false },
  ],
  "mergeModelChoices",
);

console.log("accountModels.check.ts: all checks passed");
