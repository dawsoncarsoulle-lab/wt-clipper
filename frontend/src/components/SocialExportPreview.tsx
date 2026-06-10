import { BadgeCheck, ImageIcon, Smartphone, Youtube } from "lucide-react";
import type { ClipEditorMode, SocialLayout } from "../types";
import { useI18n } from "../i18n/I18nProvider";

type SocialExportPreviewProps = {
  mode: ClipEditorMode;
  previewImageSrc?: string | null;
  layout: SocialLayout;
  title: string;
  subtitle: string;
  watermark: boolean;
};

export function SocialExportPreview({
  mode,
  previewImageSrc,
  layout,
  title,
  subtitle,
  watermark,
}: SocialExportPreviewProps) {
  const { t } = useI18n();
  if (mode !== "social_vertical") {
    return (
      <section className="editor-panel export-preview-panel">
        <div className="editor-panel-heading">
          <div>
            <div className="editor-kicker">{t("editor.preview")}</div>
            <h3>
              {mode === "youtube_horizontal"
                ? t("editor.mode.youtubeHorizontal")
                : t("editor.mode.trimOriginal")}
            </h3>
          </div>
          <Youtube className="h-5 w-5 text-ember" />
        </div>
        <div className="horizontal-export-preview static-export-preview">
          {previewImageSrc ? (
            <img src={previewImageSrc} alt="" />
          ) : (
            <StaticPreviewFallback />
          )}
        </div>
      </section>
    );
  }

  return (
    <section className="editor-panel export-preview-panel">
      <div className="editor-panel-heading">
        <div>
          <div className="editor-kicker">{t("editor.preview")}</div>
          <h3>{t("editor.mode.socialVertical")}</h3>
        </div>
        <Smartphone className="h-5 w-5 text-ember" />
      </div>
      <div className="vertical-export-preview static-export-preview">
        {layout === "vertical_blur" && previewImageSrc && (
          <img className="social-preview-bg" src={previewImageSrc} alt="" />
        )}
        {previewImageSrc ? (
          <img
            className={
              layout === "vertical_blur"
                ? "social-preview-fg"
                : "social-preview-crop"
            }
            src={previewImageSrc}
            alt=""
          />
        ) : (
          <StaticPreviewFallback />
        )}
        {title.trim().length > 0 && (
          <div className="social-preview-title">{title}</div>
        )}
        {subtitle.trim().length > 0 && (
          <div className="social-preview-subtitle">{subtitle}</div>
        )}
        {watermark && (
          <div className="social-preview-watermark">
            <BadgeCheck className="h-3.5 w-3.5" />
            WT Clip
          </div>
        )}
      </div>
    </section>
  );
}

function StaticPreviewFallback() {
  const { t } = useI18n();
  return (
    <div className="static-preview-fallback">
      <ImageIcon className="h-7 w-7" />
      <span>{t("editor.preview.static")}</span>
    </div>
  );
}
