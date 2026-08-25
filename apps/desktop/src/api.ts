import type { AdRuntime } from "./components/ads/types";
import type { Locale } from "./i18n/runtime";

export type ProviderType = "openai-chat" | "openai-responses" | "anthropic";

export interface Provider {
  provider_id: number;
  name: string;
  provider_type: ProviderType;
  base_url: string;
  api_key?: string;
  has_api_key: boolean;
  custom_headers: Record<string, string | null>;
  extra_params: Record<string, unknown>;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface ProviderInput {
  name: string;
  provider_type: ProviderType;
  base_url: string;
  api_key?: string;
  custom_headers: Record<string, string | null>;
  extra_params: Record<string, unknown>;
}

export interface Model {
  model_hash: string;
  provider_id: number;
  model_id: string;
  display_name: string;
  endpoint_type: ProviderType;
  request_url: string;
  enabled: boolean;
  sort_order: number;
  context_window_tokens: number | null;
  max_output_tokens: number | null;
  reasoning_enabled: boolean;
  reasoning_effort: string | null;
  supports_image_generation: boolean;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface ModelInput {
  model_id: string;
  display_name: string;
  endpoint_type: ProviderType;
  request_url: string;
  enabled: boolean;
  sort_order: number;
  context_window_tokens: number | null;
  max_output_tokens: number | null;
  reasoning_enabled: boolean;
  reasoning_effort: string | null;
  supports_image_generation: boolean;
}

export interface ModelConnectivityResult {
  duration_ms: number;
  first_text_ms: number | null;
  output_tokens: number;
  tokens_per_second: number;
  tokens_estimated: boolean;
  output: string;
}

export type CaState = "missing" | "untrusted" | "ready" | "invalid" | "unsupported";
export type IntegrationState = "disabled" | "enabled" | "degraded";
export interface CursorHarnessStatus {
  platform: string;
  ca: CaState;
  configured_models: number;
  enabled_models: number;
  integration: IntegrationState;
  proxy_url: string | null;
  ca_install_command: string | null;
}

export interface PortSettings {
  proxy_port: number;
  service_port: number;
}

export interface StatisticsStorage {
  bytes: number;
  call_count: number;
  trace_count: number;
}

export type ProxyMode = "system" | "custom";

export interface ProxySettings {
  mode: ProxyMode;
  address: string;
  auth_enabled: boolean;
  username: string;
  has_password: boolean;
}

export interface ProxySettingsInput {
  mode: ProxyMode;
  address: string;
  auth_enabled: boolean;
  username: string;
  password?: string;
}

export type TabMode = "public" | "direct" | "custom";

export interface TabSettings {
  mode: TabMode;
  address: string;
}

export interface OverviewMetrics {
  llm_calls: number;
  successful_calls: number;
  failed_calls: number;
  token_usage: number;
  prompt_tokens: number;
  input_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  output_tokens: number;
}

export type TokenUsageGranularity = "minute" | "hour" | "day";

export interface OverviewTokenUsageBucket {
  bucket_start_ms: number;
  input_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  output_tokens: number;
}

export interface Overview {
  metrics: OverviewMetrics;
  token_usage_granularity: TokenUsageGranularity;
  token_usage_series: OverviewTokenUsageBucket[];
}

export type ProviderSelection =
  | { kind: "existing"; provider_id: number }
  | { kind: "new"; input: ProviderInput };

export interface LlmCall {
  call_kind: "provider_llm" | "cursor_official";
  route: "local_byok" | "cursor_official";
  call_id: string;
  run_id: string;
  conversation_id: string;
  provider_call_index: number;
  model_hash: string | null;
  provider_type: string;
  provider_url: string;
  request_type: string;
  request_url: string;
  model_id: string;
  display_name: string;
  reasoning_effort: string | null;
  fast: boolean | null;
  status: string;
  finish_reason: string | null;
  created_at_ms: number;
  ttfb_ms: number | null;
  ttft_ms: number | null;
  duration_ms: number | null;
  input_tokens: number | null;
  output_tokens: number | null;
  total_tokens: number | null;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  reasoning_tokens: number | null;
  message_count: number;
  tool_count: number;
  http_status: number | null;
  error_kind: string | null;
  error_message: string | null;
  detailed: boolean;
}

export interface CallDetail {
  call: LlmCall;
  request: { headers: unknown; body: unknown; byte_count: number } | null;
  response_chunks: Array<{ seq: number; received_offset_ms: number; data: string; byte_count: number }>;
  cursor_trace: {
    trace: {
      request_id: string;
      conversation_id: string | null;
      route: "local_byok" | "cursor_official";
      model_id: string | null;
      status: string;
      request_bytes: number;
      response_bytes: number;
      response_event_count: number;
      http_status: number | null;
      received_at_ms: number;
      first_response_at_ms: number | null;
      finished_at_ms: number | null;
      error_message: string | null;
    };
    artifacts: Array<{
      seq: number;
      artifact_type: string;
      source: string;
      metadata: unknown;
      created_at_ms: number;
      byte_count: number;
      encoding: "utf8" | "base64";
      data: string;
    }>;
  } | null;
}

const packagedDesktop = "__TAURI_INTERNALS__" in window
  || window.location.protocol === "tauri:"
  || window.location.hostname === "tauri.localhost";
const API_ROOT = "/__byok-api__/api";

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  let response: Response;
  try {
    response = await fetch(`${API_ROOT}${path}`, {
      ...init,
      headers: init?.body ? { "content-type": "application/json", ...init.headers } : init?.headers,
    });
  } catch (cause) {
    throw new Error(t("无法连接本地管理服务"), { cause });
  }
  if (!response.ok) {
    const body = await response.text();
    let message = body;
    try {
      const parsed = JSON.parse(body) as { message?: unknown };
      if (typeof parsed.message === "string") message = parsed.message;
    } catch {
      // Plain-text errors are already suitable for display.
    }
    throw new Error(message || `${response.status} ${response.statusText}`);
  }
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}

export const api = {
  ads: (disabledAdIds: Iterable<string>, locale: Locale) => {
    const value = [...disabledAdIds].join(",");
    return request<AdRuntime>("/ads", {
      headers: {
        "accept-language": locale,
        ...(value ? { "disable-ad-ids": value } : {}),
      },
    });
  },
  dismissAd: (id: string, reason: string) => request<void>(`/ads/${encodeURIComponent(id)}/dismissals`, { method: "POST", body: JSON.stringify({ reason }) }),
  providers: () => request<Provider[]>("/providers"),
  createProvider: (input: ProviderInput) => request<Provider>("/providers", { method: "POST", body: JSON.stringify(input) }),
  updateProvider: (id: number, input: ProviderInput) => request<Provider>(`/providers/${id}`, { method: "PUT", body: JSON.stringify(input) }),
  deleteProvider: (id: number) => request<void>(`/providers/${id}`, { method: "DELETE" }),
  discoverModels: (id: number) => request<{ models: string[] }>(`/providers/${id}/models/discover`, { method: "POST" }),
  saveModels: (id: number, models: ModelInput[]) => request<Model[]>(`/providers/${id}/models`, { method: "POST", body: JSON.stringify({ models }) }),
  models: () => request<Model[]>("/models"),
  updateModel: (hash: string, model: ModelInput) => request<Model>(`/models/${hash}`, { method: "PUT", body: JSON.stringify(model) }),
  deleteModel: (hash: string) => request<void>(`/models/${hash}`, { method: "DELETE" }),
  testModel: (hash: string) => request<ModelConnectivityResult>(`/models/${hash}/test`, { method: "POST" }),
  overview: (filter?: { startMs: number; endMs: number; modelHashes?: string[]; providerIds?: number[] }) => {
    const params = new URLSearchParams();
    if (filter) {
      params.set("start_ms", String(filter.startMs));
      params.set("end_ms", String(filter.endMs));
      if (filter.modelHashes?.length) params.set("model_hashes", JSON.stringify(filter.modelHashes));
      if (filter.providerIds?.length) params.set("provider_ids", JSON.stringify(filter.providerIds));
    }
    const query = params.size ? `?${params}` : "";
    return request<Overview>(`/overview${query}`);
  },
  createCursorModels: (provider: ProviderSelection, models: ModelInput[]) => request<{ provider: Provider; models: Model[] }>("/harness/cursor/models", { method: "POST", body: JSON.stringify({ provider, models }) }),
  discoverCursorModels: (provider: ProviderSelection) => request<{ models: string[] }>("/harness/cursor/models/discover", { method: "POST", body: JSON.stringify({ provider }) }),
  cursorHarness: () => request<CursorHarnessStatus>("/harness/cursor/status"),
  initializeCursorCa: () => request<CursorHarnessStatus>("/harness/cursor/ca/initialize", { method: "POST" }),
  openCompactionPrompt: async () => {
    if (!packagedDesktop) throw new Error(t("请在桌面应用中打开压缩提示词配置"));
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("open_compaction_prompt");
  },
  openCursorCaInstallTerminal: async (command: string) => {
    if (!packagedDesktop) throw new Error(t("请在桌面应用中打开终端安装 CA"));
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("open_terminal_with_command", { command });
  },
  copyCursorText: async (text: string) => {
    if (!packagedDesktop) throw new Error(t("请在桌面应用中复制到系统剪贴板"));
    const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
    await writeText(text);
  },
  setCursorEnabled: (enabled: boolean) => request<CursorHarnessStatus>("/harness/cursor/enabled", { method: "PUT", body: JSON.stringify({ enabled }) }),
  calls: () => request<LlmCall[]>("/llm-calls?limit=200"),
  call: (id: string) => request<CallDetail>(`/llm-calls/${encodeURIComponent(id)}`),
  openCallDetails: async (id: string) => {
    const url = new URL(window.location.href);
    url.hash = `/calls/${encodeURIComponent(id)}`;
    await request<void>("/desktop/open-external-url", { method: "POST", body: JSON.stringify({ url: url.toString() }) });
  },
  openExternalUrl: (url: string) => request<void>("/desktop/open-external-url", { method: "POST", body: JSON.stringify({ url }) }),
  observability: () => request<{ detailed: boolean }>("/settings/observability"),
  setObservability: (detailed: boolean) => request<{ detailed: boolean }>("/settings/observability", { method: "PUT", body: JSON.stringify({ detailed }) }),
  ports: () => request<PortSettings>("/settings/ports"),
  setPorts: (settings: PortSettings) => request<PortSettings>("/settings/ports", { method: "PUT", body: JSON.stringify(settings) }),
  statisticsStorage: () => request<StatisticsStorage>("/settings/storage/statistics"),
  clearStatisticsStorage: () => request<StatisticsStorage>("/settings/storage/statistics", { method: "DELETE" }),
  proxySettings: () => request<ProxySettings>("/settings/proxy"),
  setProxySettings: (settings: ProxySettingsInput) => request<ProxySettings>("/settings/proxy", { method: "PUT", body: JSON.stringify(settings) }),
  tabSettings: () => request<TabSettings>("/settings/tab"),
  setTabSettings: (settings: TabSettings) => request<TabSettings>("/settings/tab", { method: "PUT", body: JSON.stringify(settings) }),
};
