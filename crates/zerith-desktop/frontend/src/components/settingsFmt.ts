// Small formatting helpers shared by the settings screens.
import type { Account, Preset } from "../lib/api";

export const fmt = (n: number) => n.toLocaleString("pt-BR");
/** 2000000 -> "2 mi", 500000 -> "500 mil" */
export const compact = (n: number) =>
  n.toLocaleString("pt-BR", { notation: "compact" });
/** A provider id as people say it: the preset label ("openrouter" -> "OpenRouter"), else capitalised. */
export const nameOf = (presets: Preset[], id: string) =>
  presets.find((p) => p.kind === id)?.label ??
  id.charAt(0).toUpperCase() + id.slice(1);
/** "https://api.deepseek.com/v1" -> "api.deepseek.com" */
export const host = (url: string) => url.replace(/^[a-z]+:\/\//i, "").split("/")[0];
/** "generativelanguage.googleapis.com" -> "googleapis.com"; "localhost:1234" stays. */
export const domain = (url: string) => host(url).split(".").slice(-2).join(".");

export const isOauth = (a: Account) => a.auth === "oauth" || !!a.oauth_tool;
