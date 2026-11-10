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
    contextWindow: "Context window (tokens, optional)",
    maxOutputTokens: "Max output tokens (optional)",
    supportsImages: "Supports image input",
    availableModels: "Available models",
    draftNote:
      "Base URL changes and provider deletion are not supported by the node. This URL is a page-local draft only; it is never sent or persisted.",
    keyNote:
      "Only the key you explicitly enter is submitted. When editing a provider, leave it blank to preserve the existing key. The secret is cleared on submission, even if saving fails. Saving a key does not add a discovered provider or validate remote access.",
    cancelNote:
      "Closing or editing stops waiting for the request; it does not undo a change the node may already have accepted.",
    save: "Save API Key",
    addProvider: "Save provider",
    updateProvider: "Save changes",
    deleteProvider: "Delete provider",
    deleting: "Deleting…",
    deleteConfirm: "Delete this provider configuration from models.json?",
    saving: "Saving…",
    clear: "Clear local draft",
    cleared: "Local draft cleared.",
    close: "Close",
    help: "Usage notes",
    saved:
      "The node saved the API Key. Refresh providers to read credential metadata.",
    providerSaved:
      "Provider and model saved to models.json. Restart the runtime, then refresh providers.",
    fetchModels: "Fetch models",
    fetchingModels: "Fetching models…",
    fetchedModels: "Models fetched. Select one or enter a custom model ID.",
    testConnection: "Test connection",
    testingConnection: "Testing connection…",
    connectionPassed: "Connection succeeded · {{latency}} ms · {{count}} models reported.",
    connectionFailed: "Connection test failed. Check the endpoint and API key.",
    fetchModelsError: "Could not fetch models. Check the endpoint and API key.",
    saveError:
      "API Key submission failed. Check the node connection and permissions, then enter the key again.",
    providerSaveError:
      "Provider configuration failed. Check the node connection and configuration values, then try again.",
    contextWindowError: "Context window must be a positive whole number.",
    maxOutputTokensError: "Max output tokens must be a positive whole number.",
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
    contextWindow: "上下文大小（tokens，可选）",
    maxOutputTokens: "最大输出 tokens（可选）",
    supportsImages: "支持图片输入",
    availableModels: "可用模型",
    draftNote:
      "节点暂不支持修改 Base URL 或删除供应商。此地址仅为当前页面草稿，不会发送或持久化。",
    keyNote:
      "仅提交你主动输入的 API Key；修改服务商时留空会保留已有 Key。提交时清空密钥，即使保存失败也不保留。保存密钥不代表已添加发现的供应商或通过远端验证。",
    cancelNote: "关闭或编辑只会停止等待请求，不能撤销节点可能已接受的更改。",
    save: "保存 API Key",
    addProvider: "保存服务商",
    updateProvider: "保存修改",
    deleteProvider: "删除服务商",
    deleting: "正在删除…",
    deleteConfirm: "确定从 models.json 删除这个服务商配置吗？",
    saving: "正在保存…",
    clear: "清除本地草稿",
    cleared: "已清除本地草稿。",
    close: "关闭",
    help: "使用说明",
    saved: "节点已保存 API Key。请刷新供应商以读取凭据元数据。",
    providerSaved: "服务商和模型已写入 models.json。请重启运行时后刷新供应商。",
    fetchModels: "获取模型",
    fetchingModels: "正在获取模型…",
    fetchedModels: "模型获取成功，请选择模型或手动输入模型 ID。",
    testConnection: "测试连接",
    testingConnection: "正在测试连接…",
    connectionPassed: "连接成功 · {{latency}} ms · 返回 {{count}} 个模型。",
    connectionFailed: "连接测试失败，请检查地址和 API Key。",
    fetchModelsError: "获取模型失败，请检查地址和 API Key。",
    saveError: "API Key 提交失败，请检查节点连接与配置权限后重新输入。",
    providerSaveError:
      "服务商配置失败，请检查节点连接和配置内容后重试。",
    contextWindowError: "上下文大小必须是大于 0 的整数。",
    maxOutputTokensError: "最大输出 tokens 必须是大于 0 的整数。",
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
  providerProbeRevision: number;
  providerProbeController?: AbortController;
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
  const [configIsEditable, setConfigIsEditable] = useState(false);
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
  const [contextWindow, setContextWindow] = useState("");
  const [maxOutputTokens, setMaxOutputTokens] = useState("");
  const [supportsImages, setSupportsImages] = useState(false);
  const [fetchedModels, setFetchedModels] = useState<
    Array<{
      id: string;
      ownedBy?: string | null;
      contextWindow?: number | null;
      maxTokens?: number | null;
      supportsImages?: boolean | null;
    }>
  >([]);
  const [providerProbePending, setProviderProbePending] = useState(false);
  const [providerProbeAction, setProviderProbeAction] = useState<
    "fetch" | "test" | null
  >(null);
  const [providerProbeError, setProviderProbeError] = useState(false);
  const [connectionError, setConnectionError] = useState(false);
  const [connectionResult, setConnectionResult] = useState<{
    latencyMs: number;
    modelCount: number;
  } | null>(null);
  const [savePending, setSavePending] = useState(false);
  const [saveError, setSaveError] = useState<Message | null>(null);
  const [saveNotice, setSaveNotice] = useState<Message | null>(null);
  const [saveRestart, setSaveRestart] = useState(false);
  const [deletePending, setDeletePending] = useState(false);
  const canReadModels = supported.has("get_available_models");
  const canReadState = supported.has("get_state");
  const canReadAuth = canAuth && supported.has("get_auth_status");
  const canSave = canAuth && supported.has("set_api_key");
  const canAddProvider = canAuth && supported.has("set_provider_config");
  const canUpdateProvider = canAuth && supported.has("update_provider_config");
  const canDeleteProvider = canAuth && supported.has("delete_provider_config");
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
      providerProbeRevision: 0,
    };
    ownerRef.current = owner;
    setModels([]);
    setAuth([]);
    setModelPending(false);
    setModelError(null);
    setModelNotice(null);
    setModelRestart(false);
    setFetchedModels([]);
    setProviderProbePending(false);
    setProviderProbeAction(null);
    setProviderProbeError(false);
    setConnectionError(false);
    setConnectionResult(null);
    setBaseDrafts(new Map());
    configOpenRef.current = false;
    setConfigOpen(false);
    setProvider("");
    setProtocol("openai-completions");
    setSecret("");
    setBaseUrl("");
    setModelId("");
    setModelName("");
    setContextWindow("");
    setMaxOutputTokens("");
    setSupportsImages(false);
    setSavePending(false);
    setDeletePending(false);
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
      owner.providerProbeController?.abort();
      owner.providerProbeController = undefined;
    }
    setSavePending(false);
    setDeletePending(false);
    setSaveError(null);
    setSaveNotice(null);
    setSaveRestart(false);
    setProviderProbeError(false);
    setConnectionError(false);
    setConnectionResult(null);
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
    const editable = !isNew && Boolean(firstModel) && canUpdateProvider;
    setConfigIsNew(isNew);
    setConfigIsEditable(editable);
    setProvider(value);
    setProtocol(firstModel?.api || "openai-completions");
    setSecret("");
    setBaseUrl(
      isNew ? "" : editable ? firstModel?.baseUrl || "" : baseDrafts.get(value) || "",
    );
    setModelId(isNew ? "" : firstModel?.id || "");
    setModelName(isNew ? "" : firstModel?.name || "");
    setContextWindow(firstModel?.contextWindow ? String(firstModel.contextWindow) : "");
    setMaxOutputTokens(firstModel?.maxTokens ? String(firstModel.maxTokens) : "");
    setSupportsImages(firstModel?.input?.includes("image") ?? false);
    setFetchedModels([]);
    setProviderProbeError(false);
    setConnectionError(false);
    setConnectionResult(null);
    configOpenRef.current = true;
    setConfigOpen(true);
  }

  function closeConfig() {
    invalidateDraft();
    configOpenRef.current = false;
    setConfigOpen(false);
    setConfigIsNew(false);
    setConfigIsEditable(false);
    setSecret("");
    setProvider("");
    setProtocol("openai-completions");
    setBaseUrl("");
    setModelId("");
    setModelName("");
    setContextWindow("");
    setMaxOutputTokens("");
    setSupportsImages(false);
    setFetchedModels([]);
    setProviderProbeError(false);
    setConnectionError(false);
    setConnectionResult(null);
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

  async function fetchProviderModels() {
    const owner = ownerRef.current;
    const input = {
      api: protocol,
      baseUrl: baseUrl.trim(),
      apiKey: secret,
    };
    if (
      !owner ||
      !owns(owner) ||
      !configOpenRef.current ||
      (!configIsNew && !configIsEditable) ||
      (configIsNew ? !canAddProvider : !canUpdateProvider) ||
      !input.baseUrl ||
      !input.apiKey.trim() ||
      owner.providerProbeController
    )
      return;
    const revision = ++owner.providerProbeRevision;
    const controller = new AbortController();
    owner.providerProbeController = controller;
    owner.controllers.add(controller);
    const current = () =>
      owns(owner) &&
      !controller.signal.aborted &&
      configOpenRef.current &&
      (configIsNew || configIsEditable) &&
      owner.providerProbeRevision === revision;
    setProviderProbePending(true);
    setProviderProbeAction("fetch");
    setProviderProbeError(false);
    setConnectionError(false);
    setConnectionResult(null);
    try {
      const result = await api.getProviderModels(owner.sessionId, input, controller.signal);
      if (!current()) return;
      setFetchedModels(result.models);
      if (!modelId.trim() && result.models[0]) setModelId(result.models[0].id);
      if (!contextWindow.trim() && result.models[0]?.contextWindow)
        setContextWindow(String(result.models[0].contextWindow));
      if (!maxOutputTokens.trim() && result.models[0]?.maxTokens)
        setMaxOutputTokens(String(result.models[0].maxTokens));
      if (result.models[0]?.supportsImages != null)
        setSupportsImages(result.models[0].supportsImages);
    } catch {
      if (current()) setProviderProbeError(true);
    } finally {
      owner.controllers.delete(controller);
      if (owner.providerProbeController === controller)
        owner.providerProbeController = undefined;
      if (current()) {
        setProviderProbePending(false);
        setProviderProbeAction(null);
      }
    }
  }

  async function testProviderConnection() {
    const owner = ownerRef.current;
    const input = {
      api: protocol,
      baseUrl: baseUrl.trim(),
      apiKey: secret,
    };
    if (
      !owner ||
      !owns(owner) ||
      !configOpenRef.current ||
      (!configIsNew && !configIsEditable) ||
      (configIsNew ? !canAddProvider : !canUpdateProvider) ||
      !input.baseUrl ||
      !input.apiKey.trim() ||
      owner.providerProbeController
    )
      return;
    const revision = ++owner.providerProbeRevision;
    const controller = new AbortController();
    owner.providerProbeController = controller;
    owner.controllers.add(controller);
    const current = () =>
      owns(owner) &&
      !controller.signal.aborted &&
      configOpenRef.current &&
      (configIsNew || configIsEditable) &&
      owner.providerProbeRevision === revision;
    setProviderProbePending(true);
    setProviderProbeAction("test");
    setProviderProbeError(false);
    setConnectionError(false);
    setConnectionResult(null);
    try {
      const result = await api.testProviderConfig(
        owner.sessionId,
        input,
        controller.signal,
      );
      if (!current()) return;
      setConnectionResult({
        latencyMs: result.latencyMs,
        modelCount: result.modelCount ?? 0,
      });
    } catch {
      if (current()) setConnectionError(true);
    } finally {
      owner.controllers.delete(controller);
      if (owner.providerProbeController === controller)
        owner.providerProbeController = undefined;
      if (current()) {
        setProviderProbePending(false);
        setProviderProbeAction(null);
      }
    }
  }

  async function deleteProviderConfig() {
    const owner = ownerRef.current;
    if (
      !owner ||
      !owns(owner) ||
      !configOpenRef.current ||
      !configIsEditable ||
      !canDeleteProvider ||
      deletePending ||
      !window.confirm(t.deleteConfirm)
    )
      return;
    const controller = new AbortController();
    owner.saveController = controller;
    owner.controllers.add(controller);
    setDeletePending(true);
    setSaveError(null);
    try {
      await api.deleteProviderConfig(owner.sessionId, provider.trim(), controller.signal);
      if (!owns(owner) || controller.signal.aborted) return;
      closeConfig();
      void readProviders(owner);
    } catch {
      if (owns(owner) && !controller.signal.aborted) setSaveError("providerSaveError");
    } finally {
      owner.controllers.delete(controller);
      if (owner.saveController === controller) owner.saveController = undefined;
      setDeletePending(false);
    }
  }

  async function saveProvider() {
    const owner = ownerRef.current;
    const parsedContextWindow = contextWindow.trim()
      ? Number(contextWindow.trim())
      : undefined;
    const parsedMaxOutputTokens = maxOutputTokens.trim()
      ? Number(maxOutputTokens.trim())
      : undefined;
    if (
      parsedContextWindow !== undefined &&
      (!Number.isSafeInteger(parsedContextWindow) || parsedContextWindow <= 0)
    ) {
      setSaveError("contextWindowError");
      return;
    }
    if (
      parsedMaxOutputTokens !== undefined &&
      (!Number.isSafeInteger(parsedMaxOutputTokens) || parsedMaxOutputTokens <= 0)
    ) {
      setSaveError("maxOutputTokensError");
      return;
    }
    const values = {
      provider: provider.trim(),
      api: protocol,
      baseUrl: baseUrl.trim(),
      apiKey: secret,
      modelId: modelId.trim(),
      modelName: modelName.trim(),
      ...(parsedContextWindow !== undefined
        ? { contextWindow: parsedContextWindow }
        : {}),
      ...(parsedMaxOutputTokens !== undefined
        ? { maxOutputTokens: parsedMaxOutputTokens }
        : {}),
      supportsImages,
    };
    setSecret("");
    if (
      !owner ||
      !owns(owner) ||
      !configOpenRef.current ||
      (!configIsNew && !configIsEditable) ||
      (configIsNew ? !canAddProvider : !canUpdateProvider) ||
      owner.saveController ||
      !values.provider ||
      !values.baseUrl ||
      !values.modelId ||
      (configIsNew && !values.apiKey.trim())
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
      (configIsNew || configIsEditable) &&
      owner.draftRevision === revision;
    setSavePending(true);
    setSaveError(null);
    setSaveNotice(null);
    setSaveRestart(false);
    try {
      await Promise.resolve();
      if (!current()) return;
      if (configIsNew) {
        await api.setProviderConfig(owner.sessionId, values, controller.signal);
      } else {
        await api.updateProviderConfig(owner.sessionId, values, controller.signal);
      }
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
  const apiKeyField = (
    <>
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
    </>
  );
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
              readOnly={configIsEditable}
              value={provider}
              onChange={(event) => {
                invalidateDraft();
                setProvider(event.target.value);
                setSecret("");
              }}
            />
            {(configIsNew || configIsEditable) && (
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
                {apiKeyField}
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
                <div className="provider-manager-inline-actions">
                  <button
                    type="button"
                    disabled={providerProbePending || !baseUrl.trim() || !secret.trim()}
                    onClick={() => void fetchProviderModels()}
                  >
                    {providerProbeAction === "fetch"
                      ? t.fetchingModels
                      : t.fetchModels}
                  </button>
                  <button
                    type="button"
                    disabled={providerProbePending || !baseUrl.trim() || !secret.trim()}
                    onClick={() => void testProviderConnection()}
                  >
                    {providerProbeAction === "test"
                      ? t.testingConnection
                      : t.testConnection}
                  </button>
                </div>
                {fetchedModels.length > 0 && (
                  <>
                    <p role="status">{t.fetchedModels}</p>
                    <label htmlFor={`${id}-available-models`}>{t.availableModels}</label>
                    <select
                      id={`${id}-available-models`}
                      value={fetchedModels.some((model) => model.id === modelId) ? modelId : ""}
                      onChange={(event) => {
                        const selected = fetchedModels.find(
                          (model) => model.id === event.target.value,
                        );
                        setModelId(event.target.value);
                        if (selected?.contextWindow)
                          setContextWindow(String(selected.contextWindow));
                        if (selected?.maxTokens)
                          setMaxOutputTokens(String(selected.maxTokens));
                        if (selected?.supportsImages != null)
                          setSupportsImages(selected.supportsImages);
                      }}
                    >
                      <option value="">{t.availableModels}</option>
                      {fetchedModels.map((model) => (
                        <option key={model.id} value={model.id}>
                          {model.id}
                        </option>
                      ))}
                    </select>
                  </>
                )}
                {providerProbeError && (
                  <p className="provider-manager-error" role="alert">
                    {t.fetchModelsError}
                  </p>
                )}
                {connectionError && (
                  <p className="provider-manager-error" role="alert">
                    {t.connectionFailed}
                  </p>
                )}
                {connectionResult && (
                  <p role="status">
                    {t.connectionPassed
                      .replace("{{latency}}", String(connectionResult.latencyMs))
                      .replace("{{count}}", String(connectionResult.modelCount))}
                  </p>
                )}
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
                <label htmlFor={`${id}-context-window`}>{t.contextWindow}</label>
                <input
                  id={`${id}-context-window`}
                  type="number"
                  min="1"
                  step="1"
                  inputMode="numeric"
                  value={contextWindow}
                  placeholder="128000"
                  onChange={(event) => {
                    invalidateDraft();
                    setContextWindow(event.target.value);
                  }}
                />
                <label htmlFor={`${id}-max-output-tokens`}>{t.maxOutputTokens}</label>
                <input
                  id={`${id}-max-output-tokens`}
                  type="number"
                  min="1"
                  step="1"
                  inputMode="numeric"
                  value={maxOutputTokens}
                  placeholder="4096"
                  onChange={(event) => {
                    invalidateDraft();
                    setMaxOutputTokens(event.target.value);
                  }}
                />
                <label htmlFor={`${id}-supports-images`}>
                  <input
                    id={`${id}-supports-images`}
                    type="checkbox"
                    checked={supportsImages}
                    onChange={(event) => {
                      invalidateDraft();
                      setSupportsImages(event.target.checked);
                    }}
                  />{" "}
                  {t.supportsImages}
                </label>
              </>
            )}
            {!configIsNew && !configIsEditable && (
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
            {!configIsNew && !configIsEditable && apiKeyField}
            <button
              type="button"
              aria-label={
                configIsNew
                  ? t.addProvider
                  : configIsEditable
                    ? t.updateProvider
                    : t.save
              }
              disabled={
                savePending ||
                deletePending ||
                (configIsNew || configIsEditable
                  ? (configIsNew ? !canAddProvider : !canUpdateProvider) ||
                    !provider.trim() ||
                    !baseUrl.trim() ||
                    !modelId.trim() ||
                    (configIsNew && !secret.trim())
                  : !canSave || !provider.trim() || !secret.trim())
              }
              onClick={() =>
                void (configIsNew || configIsEditable ? saveProvider() : saveKey())
              }
            >
              {savePending
                ? t.saving
                : configIsNew
                  ? t.addProvider
                  : configIsEditable
                    ? t.updateProvider
                    : t.save}
            </button>
            {configIsNew && !canAddProvider && (
              <p className="provider-manager-note">{t.providerUnsupported}</p>
            )}
            {configIsEditable && !canUpdateProvider && (
              <p className="provider-manager-note">{t.providerUnsupported}</p>
            )}
            {!configIsNew && !configIsEditable && !canSave && (
              <p className="provider-manager-note">{t.keyUnsupported}</p>
            )}
            {saveError && (
              <p className="provider-manager-error" role="alert">
                {t[configIsNew || configIsEditable ? "providerSaveError" : saveError]}
              </p>
            )}
            {saveNotice && <p role="status">{t[saveNotice]}</p>}
            {saveRestart && <p role="status">{t.restart}</p>}
            {!configIsNew && !configIsEditable && (
              <p className="provider-manager-note">{t.draftNote}</p>
            )}
            {configIsEditable && (
              <button
                type="button"
                className="provider-manager-danger"
                disabled={savePending || deletePending || !canDeleteProvider}
                onClick={() => void deleteProviderConfig()}
              >
                {deletePending ? t.deleting : t.deleteProvider}
              </button>
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
