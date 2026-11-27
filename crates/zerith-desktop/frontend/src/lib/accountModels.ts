import type { Account } from "./api.ts";
import { catalogEntry, NOT_CHAT } from "./catalog.ts";

export interface ModelChoice {
  id: string;
  name: string;
  fromApi: boolean;
}

/** Deduplicates and trims a list of model IDs while preserving order. */
export function normalizeModelList(ids: readonly string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of ids) {
    const id = raw.trim();
    if (!id || seen.has(id)) continue;
    seen.add(id);
    out.push(id);
  }
  return out;
}

/**
 * Resolves the model IDs an account exposes in Chat and Delegation pickers.
 *
 * - Non-empty `enabled_models` acts as an exact WYSIWYG whitelist (preserving
 *   order and never stripping explicitly selected models with `NOT_CHAT`).
 * - Empty `enabled_models` preserves backward-compatible defaults:
 *   OAuth live models if available, else `[default_model, ...extraIds, ...catalog]`,
 *   filtered to chat-capable models (`!NOT_CHAT`).
 */
export function resolveAccountModelIds(
  account: Pick<
    Account,
    "provider" | "oauth_tool" | "default_model" | "enabled_models"
  >,
  opts?: {
    live?: readonly { id: string }[];
    extraIds?: readonly (string | null | undefined)[];
  },
): string[] {
  const curated = normalizeModelList(account.enabled_models ?? []);
  if (curated.length > 0) return curated;

  const live = opts?.live;
  const cat = catalogEntry(account.oauth_tool || account.provider)?.models ?? [];
  const raw = live?.length
    ? live.map((m) => m.id)
    : [
        account.default_model,
        ...(opts?.extraIds ?? []),
        ...cat.map((m) => m.id),
      ];

  const seen = new Set<string>();
  const out: string[] = [];
  for (const item of raw) {
    const id = item?.trim();
    if (!id || NOT_CHAT.test(id) || seen.has(id)) continue;
    seen.add(id);
    out.push(id);
  }
  return out;
}

/**
 * Merges live models returned by the provider API, static catalogue suggestions,
 * and already-selected / default IDs into a deduplicated choice list for the
 * Account Model Manager UI.
 */
export function mergeModelChoices(params: {
  liveIds?: readonly string[];
  catalogModels?: readonly { id: string; name: string }[];
  selectedIds?: readonly string[];
  defaultModel?: string | null;
}): ModelChoice[] {
  const catNames = new Map<string, string>();
  for (const m of params.catalogModels ?? []) {
    if (m.id && !catNames.has(m.id)) catNames.set(m.id, m.name);
  }

  const seen = new Set<string>();
  const out: ModelChoice[] = [];
  const push = (rawId: string | null | undefined, fromApi: boolean) => {
    const id = rawId?.trim();
    if (!id || seen.has(id)) return;
    seen.add(id);
    out.push({ id, name: catNames.get(id) ?? id, fromApi });
  };

  for (const id of params.liveIds ?? []) push(id, true);
  for (const m of params.catalogModels ?? []) push(m.id, false);
  for (const id of params.selectedIds ?? []) push(id, false);
  push(params.defaultModel, false);
  return out;
}
