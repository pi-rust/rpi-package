import { useEffect, useState } from "react";
import { Check, ChevronDown, Copy, FileText, X } from "lucide-react";
import { translate, type Language } from "./i18n";
import { userMessageParts } from "./attachments";

function fileSize(size: number, language: Language) {
  const units = ["B", "KB", "MB", "GB"];
  const unit =
    size > 0 ? Math.min(3, Math.floor(Math.log(size) / Math.log(1024))) : 0;
  return `${new Intl.NumberFormat(language === "zh" ? "zh-CN" : "en-US", { maximumFractionDigits: 1 }).format(size / 1024 ** Math.max(0, unit))} ${units[Math.max(0, unit)]}`;
}

export function AttachmentCard({
  name,
  path,
  size,
  previewUrl,
  onRemove,
  language,
}: {
  name: string;
  path: string;
  size?: number;
  previewUrl?: string;
  onRemove?: () => void;
  language: Language;
}) {
  const t = (key: Parameters<typeof translate>[1]) => translate(language, key);
  const extension =
    /\.([a-z\d]{1,12})$/i.exec(name)?.[1].toUpperCase() ?? t("file");
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1600);
    return () => window.clearTimeout(timer);
  }, [copied]);
  const header = (
    <>
      {previewUrl ? (
        <img src={previewUrl} alt="" />
      ) : (
        <span className="attachment-icon" aria-hidden="true">
          <FileText size={20} />
        </span>
      )}
      <span className="attachment-info">
        <bdi className="attachment-name" title={name}>
          {name}
        </bdi>
        <span className="attachment-meta">
          {extension}
          {typeof size === "number" && Number.isFinite(size) && size >= 0
            ? ` · ${fileSize(size, language)}`
            : ""}
        </span>
      </span>
    </>
  );
  if (onRemove)
    return (
      <div className="attachment-card composer-attachment">
        {header}
        <button
          type="button"
          className="icon-button"
          aria-label={`${t("removeAttachment")}: ${name}`}
          title={t("removeAttachment")}
          onClick={onRemove}
        >
          <X size={14} />
        </button>
      </div>
    );
  return (
    <details className="attachment-card message-attachment">
      <summary
        title={t("viewAttachmentPath")}
        aria-label={`${name} · ${t("viewAttachmentPath")}`}
      >
        {header}
        <ChevronDown
          size={14}
          className="attachment-chevron"
          aria-hidden="true"
        />
      </summary>
      <div className="attachment-path">
        <code>{path}</code>
        <button
          type="button"
          className="attachment-copy"
          aria-label={copied ? t("copied") : t("copyAttachmentPath")}
          onClick={async () => {
            try {
              await navigator.clipboard.writeText(path);
              setCopied(true);
              setCopyFailed(false);
            } catch {
              setCopyFailed(true);
            }
          }}
        >
          {copied ? <Check size={13} /> : <Copy size={13} />}
          {copied ? t("copied") : t("copyAttachmentPath")}
        </button>
        {copyFailed && <span role="status">{t("attachmentCopyFailed")}</span>}
      </div>
    </details>
  );
}

/** Display-only projection; native user text and submitted paths stay intact. */
export function UserMessageBody({
  text,
  language,
}: {
  text: string;
  language: Language;
}) {
  return (
    <div className="user-message-content">
      {userMessageParts(text).map((part, index) =>
        part.kind === "text" ? (
          <div className="prose plain-message" key={`text-${index}`}>
            <p>{part.text}</p>
          </div>
        ) : (
          <AttachmentCard
            key={`file-${index}-${part.path}`}
            name={part.name}
            path={part.path}
            language={language}
          />
        ),
      )}
    </div>
  );
}
