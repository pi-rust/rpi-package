import { useEffect, useRef, useState } from "react";
import { AlertTriangle, RefreshCw } from "lucide-react";
import { api, type Project, type ResourceCatalog } from "./api";
import { translate, type Language, type MessageKey } from "./i18n";

type Scope = "global" | "project";

type CatalogState = {
  data?: ResourceCatalog;
  error?: string;
  loading: boolean;
};

/** Read-only view of the native `rpi resource list` output for both scopes. */
export function ResourcePanel({
  project,
  language,
}: {
  project: Project;
  language: Language;
}) {
  const t = (key: MessageKey) => translate(language, key);
  const [catalogs, setCatalogs] = useState<Record<Scope, CatalogState>>({
    global: { loading: true },
    project: { loading: true },
  });
  const controller = useRef<AbortController | null>(null);

  const load = (target: AbortController) => {
    for (const scope of ["global", "project"] as const) {
      setCatalogs((previous) => ({
        ...previous,
        [scope]: { ...previous[scope], loading: true, error: undefined },
      }));
      void api
        .resources(project.id, scope, target.signal)
        .then((data) => {
          if (target.signal.aborted) return;
          setCatalogs((previous) => ({
            ...previous,
            [scope]: { data, loading: false },
          }));
        })
        .catch((cause) => {
          if (target.signal.aborted) return;
          setCatalogs((previous) => ({
            ...previous,
            [scope]: {
              loading: false,
              error: cause instanceof Error ? cause.message : String(cause),
            },
          }));
        });
    }
  };

  useEffect(() => {
    const current = new AbortController();
    controller.current = current;
    load(current);
    return () => {
      current.abort();
      if (controller.current === current) controller.current = null;
    };
  }, [project.id]);

  const refresh = () => {
    controller.current?.abort();
    const next = new AbortController();
    controller.current = next;
    load(next);
  };

  return (
    <section className="diagnostic-section resource-panel">
      <h3 className="panel-heading">
        <span>{t("resources")}</span>
        <span className="panel-heading-actions">
          <button
            type="button"
            className="text-button"
            aria-label={t("refreshResources")}
            title={t("refreshResources")}
            onClick={refresh}
          >
            <RefreshCw size={12} /> {t("refresh")}
          </button>
        </span>
      </h3>
      <p className="muted resource-hint">
        <AlertTriangle size={12} /> {t("resourcesRestartHint")}
      </p>
      {(["global", "project"] as const).map((scope) => (
        <div className="resource-scope" key={scope}>
          <h4>{t(scope === "global" ? "resourceGlobal" : "resourceProject")}</h4>
          <CatalogView state={catalogs[scope]} language={language} />
        </div>
      ))}
    </section>
  );
}

function CatalogView({
  state,
  language,
}: {
  state: CatalogState;
  language: Language;
}) {
  const t = (key: MessageKey) => translate(language, key);
  if (state.loading && !state.data) return <p className="muted">{t("loading")}</p>;
  if (state.error)
    return (
      <p className="muted resource-error" role="alert">
        {state.error}
      </p>
    );
  const rows = state.data?.data?.resources ?? [];
  if (!rows.length) return <p className="muted">{t("resourcesEmpty")}</p>;
  return (
    <ul className="resource-list">
      {rows.map((row, index) => (
        <li className="resource-item" key={row.identity ?? `${row.name}-${index}`}>
          <div className="resource-item-head">
            <strong>{row.name ?? row.identity ?? "—"}</strong>
            <span className="resource-kind">{row.kind ?? "—"}</span>
          </div>
          <div className="resource-item-meta">
            {row.version && <span>v{row.version}</span>}
            {row.inUse !== undefined && (
              <span>{t(row.inUse ? "resourceInUse" : "resourceNotInUse")}</span>
            )}
            {row.loaded !== null && row.loaded !== undefined && (
              <span>{t(row.loaded ? "resourceLoaded" : "resourceNotLoaded")}</span>
            )}
            {row.ownership && <span>{row.ownership}</span>}
          </div>
          {row.path && <code className="resource-path">{row.path}</code>}
          {row.diagnostics?.map((d, i) => (
            <small className="resource-diagnostic" key={i}>{d}</small>
          ))}
        </li>
      ))}
    </ul>
  );
}
