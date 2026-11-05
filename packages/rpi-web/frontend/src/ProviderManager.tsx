import { useEffect, useId, useRef, useState } from "react";
import {
  api,
  commandData,
  type AuthProviderStatus,
  type ModelDescriptor,
  type RuntimeState,
} from "./api";
import type { Language } from "./i18n";
import { Modal } from "./ProjectSidebar";
import "./provider-manager.css";

const copy = {
  en: {
    title: "Model providers",
    back: "Back to runtime settings",
    refresh: "Refresh providers",
    add: "Add connection",
    intro:
      "Models are discovered by the current node. Credential metadata indicates local readiness only, not remote API Key verification.",
    loading: "Reading providers…",
    empty:
      "No providers reported. Configure providers on the node, then refresh.",
    models: "Models",
    noModels: "No available models reported for this provider.",
    source: "Credential source",
    ready: "Locally ready",
    notReady: "Not locally ready",
    unknown: "Not reported",
    oauth: "OAuth is managed on the node; no login is reported.",
    current: "Current model",
    use: "Use model",
    useLabel: "Use model: ",
    settings: "Connection settings",
    settingsLabel: "Connection settings: ",
    provider: "Provider ID",
    protocol: "Protocol",
    openaiCompletions: "OpenAI-compatible Chat Completions",
    openaiResponses: "OpenAI Responses",
    anthropicMessages: "Anthropic Messages",
    key: "API Key",
    base: "Base URL",
    baseDraft: "Custom Base URL · local draft",
    modelId: "Model ID",
    modelName: "Model name (optional)",
    draftNote:
      "Base URL changes and provider deletion are not supported by the node. This URL is a page-local draft only; it is never sent or persisted.",
    keyNote:
      "Only the key you explicitly enter is submitted. The secret is cleared on submission, even if saving fails. Saving a key does not add a discovered provider or validate remote access.",
    cancelNote:
      "Closing or editing stops waiting for the request; it does not undo a change the node may already have accepted.",
    save: "Save API Key",
    addProvider: "Save provider",
    saving: "Saving…",
    clear: "Clear local draft",
    cleared: "Local draft cleared.",
    close: "Close",
    help: "Usage notes",
    saved:
      "The node saved the API Key. Refresh providers to read credential metadata.",
    providerSaved:
      "Provider and model saved to models.json. Restart the runtime, then refresh providers.",
    saveError:
      "API Key submission failed. Check the node connection and permissions, then enter the key again.",
    providerSaveError:
      "Provider configuration failed. Check the node connection and configuration values, then try again.",
    restart:
      "The node requires a runtime restart for this change. Restart on the node, then refresh; this page will not restart it automatically.",
    modelSaved: "Model selection submitted and runtime state read back.",
    modelError:
      "Model selection failed. Check the node connection before trying again.",
    stateAfterModelError:
      "Model selection was submitted, but reading runtime state failed. Refresh to check the result.",
    modelsError: "Could not read available models.",
    authError: "Could not read credential metadata.",
    stateError: "Could not read runtime state.",
    modelsUnsupported: "This node does not support reading available models.",
    authUnsupported:
      "Credential metadata is unavailable on this node. Models can still be viewed; credentials must be managed on the node when API Key configuration is unavailable.",
    keyUnsupported:
      "This node does not support API Key configuration here. Configure credentials on the node.",
    providerUnsupported:
      "This node does not support adding provider configurations. Configure models.json on the node.",
    modelUnsupported:
      "Model selection requires both set_model and get_state support on this node.",
    stateUnsupported: "This node does not support reading runtime state.",
    reasoning: "Reasoning",
    context: "Context tokens",
  },
  zh: {
    title: "模型供应商",
    back: "返回运行时设置",
    refresh: "刷新供应商",
    add: "添加连接",
    intro:
      "模型来自当前节点的真实发现结果；凭据元数据仅表示本机就绪情况，不代表远端已验证 API Key。",
    loading: "正在读取供应商…",
    empty: "暂无节点报告的供应商。请在节点端配置供应商后刷新。",
    models: "模型",
    noModels: "该供应商暂无节点报告的可用模型。",
    source: "凭据来源",
    ready: "本机已就绪",
    notReady: "本机未就绪",
    unknown: "未报告",
    oauth: "OAuth 由节点端管理，当前未报告为已登录。",
    current: "当前模型",
    use: "使用模型",
    useLabel: "使用模型：",
    settings: "连接设置",
    settingsLabel: "连接设置：",
    provider: "Provider 标识",
    protocol: "协议",
    openaiCompletions: "OpenAI 兼容 Chat Completions",
    openaiResponses: "OpenAI Responses",
    anthropicMessages: "Anthropic Messages",
    key: "API Key",
    base: "Base URL",
    baseDraft: "自定义 Base URL · 本地草稿",
    modelId: "模型 ID",
    modelName: "模型名称（可选）",
    draftNote:
      "节点暂不支持修改 Base URL 或删除供应商。此地址仅为当前页面草稿，不会发送或持久化。",
    keyNote:
      "仅提交你主动输入的 API Key；提交时清空密钥，即使保存失败也不保留。保存密钥不代表已添加发现的供应商或通过远端验证。",
    cancelNote: "关闭或编辑只会停止等待请求，不能撤销节点可能已接受的更改。",
    save: "保存 API Key",
    addProvider: "保存服务商",
    saving: "正在保存…",
    clear: "清除本地草稿",
    cleared: "已清除本地草稿。",
    close: "关闭",
    help: "使用说明",
    saved: "节点已保存 API Key。请刷新供应商以读取凭据元数据。",
    providerSaved: "服务商和模型已写入 models.json。请重启运行时后刷新供应商。",
    saveError: "API Key 提交失败，请检查节点连接与配置权限后重新输入。",
    providerSaveError:
      "服务商配置失败，请检查节点连接和配置内容后重试。",
    restart:
      "节点要求重启运行时后生效。请在节点端重启，然后刷新检测；此页面不会自动重启。",
    modelSaved: "模型选择已提交并回读运行状态。",
    modelError: "设置模型失败，请检查节点连接后重试。",
    stateAfterModelError: "模型选择已提交，但回读运行状态失败，请刷新检测。",
    modelsError: "读取可用模型失败。",
    authError: "读取凭据元数据失败。",
    stateError: "读取运行状态失败。",
    modelsUnsupported: "当前节点未提供读取可用模型能力。",
    authUnsupported:
      "当前节点无法读取凭据元数据，仍可查看模型；若不支持 API Key 配置，请在节点端管理凭据。",
    keyUnsupported: "当前节点未提供 API Key 配置能力，请在节点端配置凭据。",
    providerUnsupported:
      "当前节点未提供添加服务商配置的能力，请在节点端配置 models.json。",
    modelUnsupported: "模型切换要求当前节点同时支持 set_model 和 get_state。",
    stateUnsupported: "当前节点未提供读取运行状态能力。",
    reasoning: "推理",
    context: "上下文 tokens",
  },
} as const;

type Message = keyof (typeof copy)["en"];
type Owner = {
  sessionId: string;
  active: boolean;
  controllers: Set<AbortController>;
  readRevision: number;
  modelRevision: number;
  draftRevision: number;
  saveController?: AbortController;
  modelController?: AbortController;
};

function dispose(owner: Owner) {
  owner.active = false;
  for (const controller of owner.controllers) controller.abort();
  owner.controllers.clear();
}

export interface ProviderManagerProps {
  sessionId: string;
  language: Language;
  supported: Set<string>;
  canAuth: boolean;
  runtime: RuntimeState | null;
  onChange: (state: RuntimeState) => void;
  onBack: () => void;
}

export function ProviderManager({
  sessionId,
  language,
  supported,
  canAuth,
  runtime,
  onChange,
  onBack,
}: ProviderManagerProps) {
  const t = copy[language];
  const id = useId();
  const ownerRef = useRef<Owner | null>(null);
  // The composer can change runtime while an independent discovery read is held.
  const latest = useRef({ sessionId, onChange, runtime });
  latest.current = { sessionId, onChange, runtime };
  const [models, setModels] = useState<ModelDescriptor[]>([]);
  const [auth, setAuth] = useState<AuthProviderStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [readErrors, setReadErrors] = useState<Message[]>([]);
  const [modelPending, setModelPending] = useState(false);
  const [modelError, setModelError] = useState<Message | null>(null);
  const [modelNotice, setModelNotice] = useState<Message | null>(null);
  const [modelRestart, setModelRestart] = useState(false);
  const [configOpen, setConfigOpen] = useState(false);
  const [configIsNew, setConfigIsNew] = useState(false);
  const configOpenRef = useRef(false);
  const dialogBody = useRef<HTMLDivElement>(null);
  const returnFocus = useRef<HTMLElement | null>(null);
  useEffect(() => {
    if (!configOpen) return;
    const trapTab = (event: KeyboardEvent) => {
      if (event.key !== "Tab") return;
      const dialog = dialogBody.current?.closest('[role="dialog"]');
      const controls = Array.from(
        dialog?.querySelectorAll<HTMLElement>(
          'button:not(:disabled), input:not(:disabled), [tabindex="0"]',
        ) ?? [],
      ).filter((control) => control.getClientRects().length > 0);
      const first = controls[0];
      const last = controls.at(-1);
      if (!first || !last) return;
      if (
        event.shiftKey &&
        (document.activeElement === first ||
          !dialog?.contains(document.activeElement))
      ) {
        event.preventDefault();
        last.focus();
      } else if (
        !event.shiftKey &&
        (document.activeElement === last ||
          !dialog?.contains(document.activeElement))
      ) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", trapTab);
    return () => {
      document.removeEventListener("keydown", trapTab);
      if (returnFocus.current?.isConnected)
        returnFocus.current.focus({ preventScroll: true });
    };
  }, [configOpen]);
  const [provider, setProvider] = useState("");
  const [protocol, setProtocol] = useState("openai-completions");
  const [secret, setSecret] = useState("");
  const [baseDrafts, setBaseDrafts] = useState(() => new Map<string, string>());
  const [baseUrl, setBaseUrl] = useState("");
  const [modelId, setModelId] = useState("");
  const [modelName, setModelName] = useState("");
  const [savePending, setSavePending] = useState(false);
  const [saveError, setSaveError] = useState<Message | null>(null);
  const [saveNotice, setSaveNotice] = useState<Message | null>(null);
  const [saveRestart, setSaveRestart] = useState(false);
  const canReadModels = supported.has("get_available_models");
  const canReadState = supported.has("get_state");
  const canReadAuth = canAuth && supported.has("get_auth_status");
  const canSave = canAuth && supported.has("set_api_key");
  const canAddProvider = canAuth && supported.has("set_provider_config");
  const canSelect = supported.has("set_model") && canReadState;

  const owns = (owner: Owner) =>
    owner.active &&
    ownerRef.current === owner &&
    latest.current.sessionId === owner.sessionId;

  async function readProviders(owner: Owner) {
    if (!owns(owner)) return;
    const revision = ++owner.readRevision;
    const runtimeAtStart = latest.current.runtime;
    const controller = new AbortController();
    owner.controllers.add(controller);
    const current = () =>
      owns(owner) &&
      !controller.signal.aborted &&
      owner.readRevision === revision;
    setLoading(true);
    setReadErrors([]);
    // Defer I/O so StrictMode's provisional effect mount can clean up first.
    await Promise.resolve();
    if (!current()) {
      owner.controllers.delete(controller);
      return;
    }
    const [modelResult, authResult, stateResult] = await Promise.allSettled([
      canReadModels
        ? api.models(owner.sessionId, controller.signal)
        : Promise.resolve(null),
      canReadAuth
        ? api.authStatus(owner.sessionId, controller.signal)
        : Promise.resolve(null),
      canReadState
        ? api.runtimeState(owner.sessionId, controller.signal)
        : Promise.resolve(null),
    ]);
    owner.controllers.delete(controller);
    if (!current()) return;
    const errors: Message[] = [];
    if (modelResult.status === "fulfilled")
      setModels(modelResult.value?.models ?? []);
    else {
      setModels([]);
      errors.push("modelsError");
    }
    if (authResult.status === "fulfilled")
      setAuth(authResult.value?.providers ?? []);
    else {
      setAuth([]);
      errors.push("authError");
    }
    if (stateResult.status === "rejected") errors.push("stateError");
    setReadErrors(errors);
    setLoading(false);
    if (
      stateResult.status === "fulfilled" &&
      stateResult.value &&
      current() &&
      latest.current.runtime === runtimeAtStart
    )
      latest.current.onChange(stateResult.value);
  }

  useEffect(() => {
    const owner: Owner = {
      sessionId,
      active: true,
      controllers: new Set(),
      readRevision: 0,
      modelRevision: 0,
      draftRevision: 0,
    };
    ownerRef.current = owner;
    setModels([]);
    setAuth([]);
    setModelPending(false);
    setModelError(null);
    setModelNotice(null);
    setModelRestart(false);
    setBaseDrafts(new Map());
    configOpenRef.current = false;
    setConfigOpen(false);
    setProvider("");
    setProtocol("openai-completions");
    setSecret("");
    setBaseUrl("");
    setModelId("");
    setModelName("");
    setSavePending(false);
    setSaveError(null);
    setSaveNotice(null);
    setSaveRestart(false);
    void readProviders(owner);
    return () => dispose(owner);
    // Capability booleans, not Set identity: parent state updates must not reset drafts.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, canReadModels, canReadAuth, canReadState, canSave, canSelect]);

  function invalidateDraft() {
    const owner = ownerRef.current;
    if (owner) {
      ++owner.draftRevision;
      owner.saveController?.abort();
      owner.saveController = undefined;
    }
    setSavePending(false);
    setSaveError(null);
    setSaveNotice(null);
    setSaveRestart(false);
  }

  function openConfig(value: string) {
    const owner = ownerRef.current;
    if (!owner || !owns(owner) || loading || owner.modelController) return;
    invalidateDraft();
    const isNew = !value.trim();
    const firstModel = models.find((model) => model.provider === value);
    returnFocus.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    setConfigIsNew(isNew);
    setProvider(value);
    setProtocol(firstModel?.api || "openai-completions");
    setSecret("");
    setBaseUrl(isNew ? "" : baseDrafts.get(value) || "");
    setModelId(isNew ? "" : firstModel?.id || "");
    setModelName(isNew ? "" : firstModel?.name || "");
    configOpenRef.current = true;
    setConfigOpen(true);
  }

  function closeConfig() {
    invalidateDraft();
    configOpenRef.current = false;
    setConfigOpen(false);
    setConfigIsNew(false);
    setSecret("");
    setProvider("");
    setProtocol("openai-completions");
    setBaseUrl("");
    setModelId("");
    setModelName("");
  }

  async function saveKey() {
    const owner = ownerRef.current;
    const chosenProvider = provider.trim();
    const key = secret;
    // Explicit submission never leaves the secret in the input, including failures.
    setSecret("");
    if (
      !owner ||
      !owns(owner) ||
      !configOpenRef.current ||
      !canSave ||
      owner.saveController ||
      !chosenProvider ||
      !key.trim()
    )
      return;
    const revision = ++owner.draftRevision;
    const controller = new AbortController();
    owner.saveController = controller;
    owner.controllers.add(controller);
    const current = () =>
      owns(owner) &&
      !controller.signal.aborted &&
      configOpenRef.current &&
      owner.draftRevision === revision;
    setSavePending(true);
    setSaveError(null);
    setSaveNotice(null);
    setSaveRestart(false);
    try {
      await Promise.resolve();
      if (!current()) return;
      const result = await api.setApiKey(
        owner.sessionId,
        chosenProvider,
        key,
        controller.signal,
      );
      if (!current()) return;
      setSaveNotice("saved");
      setSaveRestart(result.requiresRestart === true);
    } catch {
      // Never render server error text: it could echo credentials.
      if (current()) setSaveError("saveError");
    } finally {
      owner.controllers.delete(controller);
      if (owner.saveController === controller) owner.saveController = undefined;
      if (current()) setSavePending(false);
    }
  }

  async function saveProvider() {
    const owner = ownerRef.current;
    const values = {
      provider: provider.trim(),
      api: protocol,
      baseUrl: baseUrl.trim(),
      apiKey: secret,
      modelId: modelId.trim(),
      modelName: modelName.trim(),
    };
    setSecret("");
    if (
      !owner ||
      !owns(owner) ||
      !configOpenRef.current ||
      !configIsNew ||
      !canAddProvider ||
      owner.saveController ||
      !values.provider ||
      !values.baseUrl ||
      !values.apiKey.trim() ||
      !values.modelId
    )
      return;
    const revision = ++owner.draftRevision;
    const controller = new AbortController();
    owner.saveController = controller;
    owner.controllers.add(controller);
    const current = () =>
      owns(owner) &&
      !controller.signal.aborted &&
      configOpenRef.current &&
      configIsNew &&
      owner.draftRevision === revision;
    setSavePending(true);
    setSaveError(null);
    setSaveNotice(null);
    setSaveRestart(false);
    try {
      await Promise.resolve();
      if (!current()) return;
      await api.setProviderConfig(owner.sessionId, values, controller.signal);
      if (!current()) return;
      setSaveNotice("providerSaved");
      setSaveRestart(true);
    } catch {
      if (current()) setSaveError("saveError");
    } finally {
      owner.controllers.delete(controller);
      if (owner.saveController === controller) owner.saveController = undefined;
      if (current()) setSavePending(false);
    }
  }

  async function selectModel(model: ModelDescriptor) {
    const owner = ownerRef.current;
    if (
      !owner ||
      !owns(owner) ||
      !canSelect ||
      loading ||
      configOpenRef.current ||
      owner.modelController ||
      owner.saveController
    )
      return;
    const revision = ++owner.modelRevision;
    const controller = new AbortController();
    owner.modelController = controller;
    owner.controllers.add(controller);
    const current = () =>
      owns(owner) &&
      !controller.signal.aborted &&
      owner.modelRevision === revision;
    setModelPending(true);
    setModelError(null);
    setModelNotice(null);
    setModelRestart(false);
    let submitted = false;
    try {
      await Promise.resolve();
      if (!current()) return;
      const result = await commandData<{ requiresRestart?: boolean }>(
        owner.sessionId,
        "set_model",
        { provider: model.provider, modelId: model.id },
        controller.signal,
      );
      if (!current()) return;
      submitted = true;
      setModelRestart(result.requiresRestart === true);
      const runtimeAtRead = latest.current.runtime;
      const state = await api.runtimeState(owner.sessionId, controller.signal);
      if (!current()) return;
      // Do not overwrite a newer state applied through the composer while this
      // read was in flight. Aborting this fetch cannot undo either model write.
      if (latest.current.runtime === runtimeAtRead) {
        setModelNotice("modelSaved");
        latest.current.onChange(state);
      }
    } catch {
      if (current())
        setModelError(submitted ? "stateAfterModelError" : "modelError");
    } finally {
      owner.controllers.delete(controller);
      if (owner.modelController === controller)
        owner.modelController = undefined;
      if (current()) setModelPending(false);
    }
  }

  const providers = [
    ...new Set([
      ...auth.map((item) => item.provider),
      ...models.map((item) => item.provider),
    ]),
  ].sort();
  const pending = loading || modelPending || savePending;
  return (
    <section className="provider-manager" aria-label={t.title}>
      <header className="provider-manager-header">
        <button
          type="button"
          aria-label={t.back}
          onClick={() => {
            const owner = ownerRef.current;
            if (owner) dispose(owner);
            setSecret("");
            configOpenRef.current = false;
            setConfigOpen(false);
            onBack();
          }}
        >
          {t.back}
        </button>
        <h2>{t.title}</h2>
        <div className="provider-manager-actions">
          <button
            type="button"
            aria-label={t.refresh}
            disabled={pending || configOpen}
            onClick={() => {
              const owner = ownerRef.current;
              if (
                owner &&
                owns(owner) &&
                !configOpenRef.current &&
                !owner.modelController &&
                !owner.saveController
              )
                void readProviders(owner);
            }}
          >
            {t.refresh}
          </button>
          <button
            type="button"
            disabled={pending || configOpen}
            onClick={() => openConfig("")}
          >
            {t.add}
          </button>
        </div>
      </header>
      <p className="provider-manager-note" role="note">
        {t.intro}
      </p>
      {!canReadModels && (
        <p className="provider-manager-note">{t.modelsUnsupported}</p>
      )}
      {!canReadAuth && (
        <p className="provider-manager-note">{t.authUnsupported}</p>
      )}
      {!canReadState && (
        <p className="provider-manager-note">{t.stateUnsupported}</p>
      )}
      {!canSave && <p className="provider-manager-note">{t.keyUnsupported}</p>}
      {!canSelect && (
        <p className="provider-manager-note">{t.modelUnsupported}</p>
      )}
      {loading && <p role="status">{t.loading}</p>}
      {readErrors.length > 0 && (
        <div className="provider-manager-error" role="alert">
          {readErrors.map((message) => (
            <p key={message}>{t[message]}</p>
          ))}
        </div>
      )}
      {modelError && (
        <p className="provider-manager-error" role="alert">
          {t[modelError]}
        </p>
      )}
      {modelNotice && <p role="status">{t[modelNotice]}</p>}
      {modelRestart && <p role="status">{t.restart}</p>}
      {!loading && providers.length === 0 && <p>{t.empty}</p>}
      <div className="provider-manager-list">
        {providers.map((name) => {
          const credential = auth.find((item) => item.provider === name);
          const available = models.filter((model) => model.provider === name);
          return (
            <article
              className="provider-manager-card"
              role="group"
              aria-label={name}
              key={name}
            >
              <div className="provider-manager-card-heading">
                <h3>{name}</h3>
                <span>
                  {credential
                    ? credential.ready
                      ? t.ready
                      : t.notReady
                    : t.unknown}
                </span>
              </div>
              <dl>
                <div>
                  <dt>{t.source}</dt>
                  <dd>{credential?.source || t.unknown}</dd>
                </div>
                <div>
                  <dt>{t.protocol}</dt>
                  <dd>{available[0]?.api || t.unknown}</dd>
                </div>
                <div>
                  <dt>{t.models}</dt>
                  <dd>{available.length}</dd>
                </div>
              </dl>
              {credential?.oauthAvailable && !credential.oauth && (
                <p className="provider-manager-note">{t.oauth}</p>
              )}
              <h4>{t.models}</h4>
              {available.length === 0 && (
                <p className="provider-manager-note">{t.noModels}</p>
              )}
              <ul className="provider-manager-models">
                {available.map((model) => {
                  const selected =
                    runtime?.provider === name && runtime.modelId === model.id;
                  return (
                    <li
                      className={
                        selected
                          ? "provider-manager-model selected"
                          : "provider-manager-model"
                      }
                      key={model.id}
                    >
                      <div>
                        <strong>{model.name || model.id}</strong>
                        <code>{model.id}</code>
                        <div className="provider-manager-note">
                          {model.reasoning && <span>{t.reasoning}</span>}
                          {model.contextWindow != null && (
                            <span>
                              {t.context}: {model.contextWindow}
                            </span>
                          )}
                        </div>
                      </div>
                      <button
                        type="button"
                        aria-label={`${t.useLabel}${name}/${model.id}`}
                        aria-pressed={selected}
                        disabled={
                          selected || pending || configOpen || !canSelect
                        }
                        onClick={() => void selectModel(model)}
                      >
                        {selected ? t.current : t.use}
                      </button>
                    </li>
                  );
                })}
              </ul>
              <button
                type="button"
                aria-label={`${t.settingsLabel}${name}`}
                disabled={pending || configOpen}
                onClick={() => openConfig(name)}
              >
                {t.settings}
              </button>
            </article>
          );
        })}
      </div>
      {configOpen && (
        <Modal title={t.settings} language={language} onClose={closeConfig}>
          <div className="provider-manager-dialog" ref={dialogBody}>
            <label htmlFor={`${id}-provider`}>{t.provider}</label>
            <input
              id={`${id}-provider`}
              autoComplete="off"
              autoCapitalize="none"
              spellCheck={false}
              value={provider}
              onChange={(event) => {
                invalidateDraft();
                setProvider(event.target.value);
                setSecret("");
              }}
            />
            {configIsNew && (
              <>
                <label htmlFor={`${id}-protocol`}>{t.protocol}</label>
                <select
                  id={`${id}-protocol`}
                  value={protocol}
                  onChange={(event) => {
                    invalidateDraft();
                    setProtocol(event.target.value);
                  }}
                >
                  <option value="openai-completions">
                    {t.openaiCompletions}
                  </option>
                  <option value="openai-responses">{t.openaiResponses}</option>
                  <option value="anthropic-messages">
                    {t.anthropicMessages}
                  </option>
                </select>
                <label htmlFor={`${id}-base`}>{t.base}</label>
                <input
                  id={`${id}-base`}
                  type="url"
                  required
                  value={baseUrl}
                  placeholder="https://api.example.com/v1"
                  onChange={(event) => {
                    invalidateDraft();
                    setBaseUrl(event.target.value);
                  }}
                />
                <label htmlFor={`${id}-model-id`}>{t.modelId}</label>
                <input
                  id={`${id}-model-id`}
                  autoComplete="off"
                  autoCapitalize="none"
                  spellCheck={false}
                  required
                  value={modelId}
                  placeholder="my-model"
                  onChange={(event) => {
                    invalidateDraft();
                    setModelId(event.target.value);
                  }}
                />
                <label htmlFor={`${id}-model-name`}>{t.modelName}</label>
                <input
                  id={`${id}-model-name`}
                  autoComplete="off"
                  value={modelName}
                  placeholder={modelId || "My Model"}
                  onChange={(event) => {
                    invalidateDraft();
                    setModelName(event.target.value);
                  }}
                />
              </>
            )}
            {!configIsNew && (
              <>
                <label htmlFor={`${id}-base`}>{t.baseDraft}</label>
                <input
                  id={`${id}-base`}
                  type="url"
                  value={baseUrl}
                  placeholder="https://api.example.com/v1"
                  onChange={(event) => {
                    invalidateDraft();
                    const value = event.target.value;
                    setBaseUrl(value);
                    setBaseDrafts((drafts) =>
                      new Map(drafts).set(provider.trim(), value),
                    );
                  }}
                />
                <button
                  type="button"
                  onClick={() => {
                    invalidateDraft();
                    setBaseUrl("");
                    setBaseDrafts((drafts) => {
                      const next = new Map(drafts);
                      next.delete(provider.trim());
                      return next;
                    });
                    setSaveNotice("cleared");
                  }}
                >
                  {t.clear}
                </button>
              </>
            )}
            <label htmlFor={`${id}-key`}>{t.key}</label>
            <input
              id={`${id}-key`}
              type="password"
              autoComplete="new-password"
              value={secret}
              onChange={(event) => {
                invalidateDraft();
                setSecret(event.target.value);
              }}
            />
            <button
              type="button"
              aria-label={configIsNew ? t.addProvider : t.save}
              disabled={
                savePending ||
                (configIsNew
                  ? !canAddProvider ||
                    !provider.trim() ||
                    !baseUrl.trim() ||
                    !modelId.trim() ||
                    !secret.trim()
                  : !canSave || !provider.trim() || !secret.trim())
              }
              onClick={() => void (configIsNew ? saveProvider() : saveKey())}
            >
              {savePending ? t.saving : configIsNew ? t.addProvider : t.save}
            </button>
            {configIsNew && !canAddProvider && (
              <p className="provider-manager-note">{t.providerUnsupported}</p>
            )}
            {!configIsNew && !canSave && (
              <p className="provider-manager-note">{t.keyUnsupported}</p>
            )}
            {saveError && (
              <p className="provider-manager-error" role="alert">
                {t[configIsNew ? "providerSaveError" : saveError]}
              </p>
            )}
            {saveNotice && <p role="status">{t[saveNotice]}</p>}
            {saveRestart && <p role="status">{t.restart}</p>}
            {!configIsNew && (
              <p className="provider-manager-note">{t.draftNote}</p>
            )}
            <details className="provider-manager-help">
              <summary>{t.help}</summary>
              <p className="provider-manager-note">{t.keyNote}</p>
              <p className="provider-manager-note">{t.cancelNote}</p>
            </details>
            <div className="provider-manager-actions">
              <button type="button" onClick={closeConfig}>
                {t.close}
              </button>
            </div>
          </div>
        </Modal>
      )}
    </section>
  );
}
