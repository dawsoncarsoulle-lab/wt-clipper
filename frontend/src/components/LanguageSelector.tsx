import { Globe2 } from "lucide-react";
import { supportedLanguages } from "../i18n/locales";
import type { LanguageCode } from "../i18n/locales";
import { useI18n } from "../i18n/I18nProvider";

export function LanguageSelector() {
  const { language, setLanguage, t } = useI18n();

  return (
    <label className="language-selector" title={t("language.label")}>
      <Globe2 size={15} />
      <span>{t("language.label")}</span>
      <select value={language} onChange={(event) => setLanguage(event.target.value as LanguageCode)}>
        {supportedLanguages.map((item) => (
          <option key={item.code} value={item.code}>
            {item.nativeLabel}
          </option>
        ))}
      </select>
    </label>
  );
}
