// Lightweight reactive pt-BR / English UI localization.
import { useEffect, useState } from "react";

export type Lang = "pt-BR" | "en";

const LANG_KEY = "zerith.lang";
const LANG_EVENT = "zerith:lang";

export function getLang(): Lang {
  try {
    return localStorage.getItem(LANG_KEY) === "en" ? "en" : "pt-BR";
  } catch {
    return "pt-BR";
  }
}

export function setLang(lang: Lang): void {
  try {
    localStorage.setItem(LANG_KEY, lang);
  } catch {
    /* ignore storage errors */
  }
  if (typeof document !== "undefined") {
    document.documentElement.lang = lang;
  }
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent<Lang>(LANG_EVENT, { detail: lang }));
  }
}

export function useLang(): Lang {
  const [lang, setLangState] = useState<Lang>(getLang);
  useEffect(() => {
    const onCustom = (e: Event) => {
      const detail = (e as CustomEvent<Lang>).detail;
      setLangState(detail === "en" ? "en" : getLang());
    };
    const onStorage = (e: StorageEvent) => {
      if (e.key === LANG_KEY) setLangState(getLang());
    };
    window.addEventListener(LANG_EVENT, onCustom);
    window.addEventListener("storage", onStorage);
    return () => {
      window.removeEventListener(LANG_EVENT, onCustom);
      window.removeEventListener("storage", onStorage);
    };
  }, []);
  return lang;
}

export function tr(lang: Lang, pt: string, en: string): string {
  return lang === "en" ? en : pt;
}
