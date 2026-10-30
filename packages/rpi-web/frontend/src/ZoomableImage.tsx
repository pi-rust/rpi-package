import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";
import { translate, type Language } from "./i18n";

function ImageViewer({
  src,
  alt,
  language,
  onClose,
}: {
  src: string;
  alt: string;
  language: Language;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);
  const [actualSize, setActualSize] = useState(false);
  useEffect(() => {
    const dialog = ref.current!;
    const previousFocus = document.activeElement;
    const overflow = document.body.style.overflow;
    dialog.showModal();
    document.body.style.overflow = "hidden";
    closeButton.current?.focus({ preventScroll: true });
    return () => {
      dialog.close();
      document.body.style.overflow = overflow;
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected)
        previousFocus.focus({ preventScroll: true });
    };
  }, []);

  return createPortal(
    <dialog
      ref={ref}
      className="image-viewer"
      aria-label={translate(language, "imageViewer")}
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") event.stopPropagation();
        if (event.key !== "Tab") return;
        const buttons = event.currentTarget.querySelectorAll<HTMLButtonElement>(
          "button:not(:disabled)",
        );
        const first = buttons[0];
        const last = buttons[buttons.length - 1];
        if (event.shiftKey && document.activeElement === first) {
          event.preventDefault();
          last?.focus();
        } else if (!event.shiftKey && document.activeElement === last) {
          event.preventDefault();
          first?.focus();
        }
      }}
    >
      <div className="image-viewer-content">
        <header className="image-viewer-toolbar">
          <h2>{alt || translate(language, "imageViewer")}</h2>
          <button
            type="button"
            onClick={() => setActualSize((value) => !value)}
          >
            {translate(
              language,
              actualSize ? "imageFitToWindow" : "imageActualSize",
            )}
          </button>
          <button
            ref={closeButton}
            type="button"
            className="icon-button"
            aria-label={translate(language, "closeDialog")}
            title={translate(language, "closeDialog")}
            onClick={onClose}
          >
            <X size={20} />
          </button>
        </header>
        <div className="image-viewer-viewport" data-actual-size={actualSize}>
          <img src={src} alt={alt} />
        </div>
      </div>
    </dialog>,
    document.body,
  );
}

/** Reuse the already validated image source; never proxy or navigate to it. */
export function ZoomableImage({
  src,
  alt = "",
  title,
  language,
}: {
  src?: string;
  alt?: string;
  title?: string;
  language: Language;
}) {
  const [expanded, setExpanded] = useState<string | null>(null);
  const open = (image: HTMLImageElement) => {
    image.focus({ preventScroll: true });
    setExpanded(image.currentSrc || src || null);
  };
  return (
    <>
      <img
        src={src || undefined}
        alt={alt}
        title={title ?? (src ? translate(language, "enlargeImage") : undefined)}
        loading="lazy"
        className={src ? "zoomable-image" : undefined}
        role={src ? "button" : undefined}
        tabIndex={src ? 0 : undefined}
        aria-label={
          src
            ? `${translate(language, "enlargeImage")}${alt ? `: ${alt}` : ""}`
            : undefined
        }
        onClick={(event) => {
          if (!src) return;
          event.preventDefault();
          event.stopPropagation();
          open(event.currentTarget);
        }}
        onKeyDown={(event) => {
          if (!src || (event.key !== "Enter" && event.key !== " ")) return;
          event.preventDefault();
          event.stopPropagation();
          open(event.currentTarget);
        }}
      />
      {expanded && (
        <ImageViewer
          src={expanded}
          alt={alt}
          language={language}
          onClose={() => setExpanded(null)}
        />
      )}
    </>
  );
}
