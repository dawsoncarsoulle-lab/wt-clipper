import { BadgeCheck, Smartphone, Youtube } from "lucide-react";
import type { ClipEditorMode, SocialLayout } from "../types";

type SocialExportPreviewProps = {
  mode: ClipEditorMode;
  videoSrc: string;
  videoMimeType: string;
  layout: SocialLayout;
  title: string;
  subtitle: string;
  watermark: boolean;
};

export function SocialExportPreview({
  mode,
  videoSrc,
  videoMimeType,
  layout,
  title,
  subtitle,
  watermark,
}: SocialExportPreviewProps) {
  if (mode !== "social_vertical") {
    return (
      <section className="editor-panel export-preview-panel">
        <div className="editor-panel-heading">
          <div>
            <div className="editor-kicker">Preview</div>
            <h3>{mode === "youtube_horizontal" ? "YouTube horizontal" : "Original coupé"}</h3>
          </div>
          <Youtube className="h-5 w-5 text-ember" />
        </div>
        <div className="horizontal-export-preview">
          <video muted playsInline preload="metadata">
            <source src={videoSrc} type={videoMimeType} />
          </video>
        </div>
      </section>
    );
  }

  return (
    <section className="editor-panel export-preview-panel">
      <div className="editor-panel-heading">
        <div>
          <div className="editor-kicker">Preview</div>
          <h3>TikTok / Reels / Shorts</h3>
        </div>
        <Smartphone className="h-5 w-5 text-ember" />
      </div>
      <div className="vertical-export-preview">
        {layout === "vertical_blur" && (
          <video className="social-preview-bg" muted playsInline preload="metadata">
            <source src={videoSrc} type={videoMimeType} />
          </video>
        )}
        <video
          className={layout === "vertical_blur" ? "social-preview-fg" : "social-preview-crop"}
          muted
          playsInline
          preload="metadata"
        >
          <source src={videoSrc} type={videoMimeType} />
        </video>
        {title.trim().length > 0 && <div className="social-preview-title">{title}</div>}
        {subtitle.trim().length > 0 && <div className="social-preview-subtitle">{subtitle}</div>}
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
