import { Search, X } from "lucide-react";
import type { Language } from "./i18n";
import type { MessageSearchResult } from "./messageSearch";
import { translate } from "./i18n";

export function MessageSearch({
  open,
  query,
  results,
  activeIndex,
  loading,
  historySupported,
  language,
  onQuery,
  onClose,
  onSelect,
}: {
  open: boolean;
  query: string;
  results: MessageSearchResult[];
  activeIndex: number;
  loading: boolean;
  historySupported: boolean;
  language: Language;
  onQuery: (value: string) => void;
  onClose: () => void;
  onSelect: (index: number) => void;
}) {
  if (!open) return null;
  const t = (key: Parameters<typeof translate>[1]) => translate(language, key);
  return (
    <section className="message-search" role="dialog" aria-label={t("messageSearch")}>
      <div className="message-search-header">
        <label className="message-search-input">
          <Search size={15} aria-hidden="true" />
          <input
            autoFocus
            value={query}
            placeholder={t("messageSearchPlaceholder")}
            aria-label={t("messageSearchPlaceholder")}
            onChange={(event) => onQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") onClose();
              if (event.key === "ArrowDown" && results.length > 0) {
                event.preventDefault();
                onSelect((activeIndex + 1) % results.length);
              }
              if (event.key === "ArrowUp" && results.length > 0) {
                event.preventDefault();
                onSelect((activeIndex - 1 + results.length) % results.length);
              }
            }}
          />
        </label>
        <button type="button" className="icon-button" aria-label={t("closeMessageSearch")} onClick={onClose}>
          <X size={15} aria-hidden="true" />
        </button>
      </div>
      {loading && <p className="message-search-status" role="status">{t("messageSearchLoading")}</p>}
      {!loading && query.trim() && results.length === 0 && (
        <p className="message-search-status">{t("messageSearchNoResults")}</p>
      )}
      {!historySupported && query.trim() && (
        <p className="message-search-hint">{t("messageSearchLoadedOnly")}</p>
      )}
      {results.length > 0 && (
        <div className="message-search-results" role="listbox" aria-label={t("messageSearchResults")}>
          {results.map((result, index) => (
            <button
              type="button"
              role="option"
              aria-selected={index === activeIndex}
              className={`message-search-result${index === activeIndex ? " selected" : ""}`}
              key={`${result.source}:${result.entryId}`}
              onClick={() => onSelect(index)}
            >
              <span className="message-search-result-index">{index + 1}</span>
              <span>{result.preview || t("nonTextMessage")}</span>
            </button>
          ))}
        </div>
      )}
      {results.length > 0 && <p className="message-search-count">{results.length} · {t("messageSearchResults")}</p>}
    </section>
  );
}
