import { createContext, useContext, useMemo, useState } from "react";
import type { ReactNode } from "react";
import { defaultLanguage, languageStorageKey, translations } from "./locales";
import type { LanguageCode } from "./locales";

type TranslationParams = Record<string, string | number | boolean | null | undefined>;

type I18nContextValue = {
  language: LanguageCode;
  setLanguage: (language: LanguageCode) => void;
  t: (key: string, params?: TranslationParams) => string;
};

const I18nContext = createContext<I18nContextValue | null>(null);

function isSupportedLanguage(value: string | null | undefined): value is LanguageCode {
  return value === "en" || value === "fr";
}

function initialLanguage(): LanguageCode {
  const saved = window.localStorage.getItem(languageStorageKey);
  if (isSupportedLanguage(saved)) {
    return saved;
  }
  return defaultLanguage;
}

function interpolate(template: string, params?: TranslationParams) {
  if (!params) {
    return template;
  }
  return template.replace(/\{\{\s*(\w+)\s*\}\}/g, (_, key: string) => String(params[key] ?? ""));
}

export function I18nProvider({ children }: { children: ReactNode }) {
  const [language, setLanguageState] = useState<LanguageCode>(() => initialLanguage());

  const value = useMemo<I18nContextValue>(() => {
    const setLanguage = (nextLanguage: LanguageCode) => {
      window.localStorage.setItem(languageStorageKey, nextLanguage);
      setLanguageState(nextLanguage);
    };

    const t = (key: string, params?: TranslationParams) => {
      const dictionary = translations[language] ?? translations[defaultLanguage];
      const fallback = translations[defaultLanguage];
      return interpolate(dictionary[key] ?? fallback[key] ?? key, params);
    };

    return { language, setLanguage, t };
  }, [language]);

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n() {
  const context = useContext(I18nContext);
  if (!context) {
    throw new Error("useI18n must be used inside I18nProvider");
  }
  return context;
}
