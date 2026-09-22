import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// ---------- 类型（与 Rust 端 serde 结构一致） ----------

export type Agent = "claude" | "codex";
export type ProtectionMode = "config" | "system_proxy" | "launcher";
export type KeychainKind = "login" | "system";

export interface ProxyStatus {
  running: boolean;
  listen: string | null;
  proxy_url: string | null;
  pac_url: string | null;
  intercept_hosts: string[];
  last_error: string | null;
  inflight: number;
  total_requests: number;
}

export interface AppSettings {
  port: number;
  intercept_hosts: string[];
  upstream_proxy: string | null;
  store_redacted_bodies: boolean;
  restore_responses: boolean;
  log_retention_days: number;
  autostart: boolean;
  start_proxy_on_launch: boolean;
  minimize_to_tray: boolean;
  active_persona: string | null;
  substitution_style: SubstitutionStyle;
  pricing: ModelPrice[];
}

export interface ModelPrice {
  model_prefix: string;
  input_per_m: number;
  output_per_m: number;
  cache_read_per_m: number;
  cache_write_per_m: number;
}

export interface PricingView {
  prices: ModelPrice[];
  customized: boolean;
}

export interface UsageTotals {
  requests: number;
  model_calls: number;
  errors: number;
  streamed: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  reasoning_tokens: number;
  redactions: number;
  tool_calls: number;
  req_bytes: number;
  resp_bytes: number;
  cost_usd: number;
  priced_ratio: number;
}

export interface UsageBucket {
  ts: string;
  requests: number;
  errors: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  reasoning_tokens: number;
  redactions: number;
  tool_calls: number;
  cost_usd: number;
}

export interface ModelUsage {
  model: string;
  provider: string;
  requests: number;
  errors: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  reasoning_tokens: number;
  tool_calls: number;
  cost_usd: number;
  priced: boolean;
  avg_duration_ms: number;
  p50_duration_ms: number;
  p95_duration_ms: number;
  p50_ttft_ms: number | null;
  cache_hit_rate: number;
  output_tps: number;
}

export interface GroupUsage {
  key: string;
  requests: number;
  errors: number;
  input_tokens: number;
  output_tokens: number;
  cost_usd: number;
}

export interface SessionUsage {
  session_id: string;
  agent: string;
  model: string;
  first_seen: string;
  last_seen: string;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  tool_calls: number;
  redactions: number;
  cost_usd: number;
}

export interface LatencyStats {
  avg_ms: number;
  p50_ms: number;
  p95_ms: number;
  p99_ms: number;
  p50_ttft_ms: number | null;
  p95_ttft_ms: number | null;
}

export interface UsageReport {
  since: string;
  until: string;
  bucket_secs: number;
  totals: UsageTotals;
  previous: UsageTotals;
  series: UsageBucket[];
  by_model: ModelUsage[];
  by_agent: GroupUsage[];
  by_provider: GroupUsage[];
  by_api: GroupUsage[];
  heatmap: number[][];
  by_hour: number[];
  tools: [string, number][];
  stop_reasons: [string, number][];
  status_codes: [string, number][];
  sessions: SessionUsage[];
  session_count: number;
  latency: LatencyStats;
}

export interface Paths {
  data_dir: string;
  db_path: string;
  ca_dir: string;
  backups_dir: string;
  claude_settings: string;
  codex_config: string;
}

export interface CaInfo {
  common_name: string;
  fingerprint_sha256: string;
  not_before: string;
  not_after: string;
  cert_path: string;
}

export interface TrustStatus {
  in_login_keychain: boolean;
  in_system_keychain: boolean;
  trusted_for_ssl: boolean;
  sha1: string;
  detail: string;
}

export interface CaView {
  info: CaInfo;
  trust: TrustStatus;
  pem: string;
}

export interface AgentInfo {
  agent: Agent;
  installed: boolean;
  binary: string | null;
  version: string | null;
  config_path: string;
  config_exists: boolean;
  config_managed_by_us: boolean;
  conflicts: string[];
  auth_mode: string | null;
}

export interface AgentActivation {
  enabled: boolean;
  mode: ProtectionMode | null;
  rollback: unknown;
  activated_at: string | null;
}

export interface AgentView {
  info: AgentInfo;
  activation: AgentActivation;
  notices: string[];
}

export interface ServiceProxyState {
  service: string;
  url: string | null;
  enabled: boolean;
}

export interface PacStatus {
  managed: boolean;
  active: boolean;
  pac_url: string;
  hosts: string[];
  services: ServiceProxyState[];
}

export interface CliStatus {
  installed: boolean;
  link_dir: string;
  target: string | null;
  links: [string, boolean][];
}

export interface Rule {
  id: string;
  name: string;
  entity_type: string;
  kind: "regex" | "local_model";
  pattern: string;
  group: number | null;
  enabled: boolean;
  builtin: boolean;
  priority: number;
  validator: "luhn" | null;
  description: string;
}

export interface SpanView {
  start: number;
  end: number;
  text: string;
  entity_type: string;
  rule_id: string;
}

export interface RuleTestResult {
  spans: SpanView[];
  redacted: string;
}

export type SubstitutionStyle = "placeholder" | "synthetic";

export interface IdentityField {
  id: string;
  label: string;
  entity_type: string;
  real_values: string[];
  alias: string;
  case_insensitive: boolean;
  enabled: boolean;
}

export interface Persona {
  id: string;
  name: string;
  description: string;
  fields: IdentityField[];
  updated_at: string;
}

export interface PersonaView {
  personas: Persona[];
  active_persona: string | null;
  substitution_style: SubstitutionStyle;
}

export interface RequestFilter {
  agent?: string | null;
  provider?: string | null;
  outcome?: string | null;
  since?: string | null;
  until?: string | null;
  search?: string | null;
  only_redacted: boolean;
  limit: number;
  offset: number;
}

export interface RequestRow {
  id: string;
  started_at: string;
  duration_ms: number;
  entry: string;
  agent: string;
  provider: string;
  api: string;
  method: string;
  host: string;
  path: string;
  status: number | null;
  req_bytes: number;
  resp_bytes: number;
  streamed: boolean;
  redaction_count: number;
  restored_count: number;
  error: string | null;
  model: string | null;
  input_tokens: number | null;
  output_tokens: number | null;
  ttft_ms: number | null;
  session_id: string | null;
}

export interface ModelCallRow {
  provider: string;
  api: string;
  model: string | null;
  response_model: string | null;
  stream: boolean;
  message_count: number;
  tool_names: string[];
  input_tokens: number | null;
  output_tokens: number | null;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  stop_reason: string | null;
  reasoning_tokens: number | null;
  reasoning_effort: string | null;
  tool_calls: string[];
}

export interface RequestDetail {
  request: RequestRow;
  model_call: ModelCallRow | null;
  redactions: { rule_id: string; entity_type: string; count: number }[];
  warnings: string[];
  redacted_body: string | null;
}

export interface Stats {
  total_requests: number;
  total_redactions: number;
  total_errors: number;
  total_input_tokens: number;
  total_output_tokens: number;
  by_entity: [string, number][];
  by_agent: [string, number][];
  by_model: [string, number][];
}

/** 后端推送的实时请求日志（RequestLog） */
export interface RequestLogEvent {
  id: string;
  started_at: string;
  duration_ms: number;
  agent: string;
  provider: string;
  api: string;
  method: string;
  host: string;
  path: string;
  status: number | null;
  redaction_events: { rule_id: string; entity_type: string; count: number }[];
  restored_count: number;
  error: string | null;
  warnings: string[];
  ttft_ms: number | null;
  session_id: string | null;
  request_meta: { model: string | null; stream: boolean; tool_names: string[]; message_count: number } | null;
  usage: {
    response_model: string | null;
    input_tokens: number | null;
    output_tokens: number | null;
    cache_read_tokens: number | null;
    cache_write_tokens: number | null;
    reasoning_tokens: number | null;
    tool_calls: string[];
    stop_reason: string | null;
  } | null;
}

// ---------- 调用封装 ----------

export const api = {
  proxyStatus: () => invoke<ProxyStatus>("proxy_status"),
  proxyStart: () => invoke<ProxyStatus>("proxy_start"),
  proxyStop: () => invoke<ProxyStatus>("proxy_stop"),
  proxyRestart: () => invoke<ProxyStatus>("proxy_restart"),

  getSettings: () => invoke<AppSettings>("get_settings"),
  saveSettings: (settings: AppSettings) => invoke<ProxyStatus>("save_settings", { settings }),
  appPaths: () => invoke<Paths>("app_paths"),
  revealPath: (path: string) => invoke<void>("reveal_path", { path }),

  caView: () => invoke<CaView>("ca_view"),
  caInstall: (kind: KeychainKind) => invoke<TrustStatus>("ca_install", { kind }),
  caRemove: (kind: KeychainKind) => invoke<TrustStatus>("ca_remove", { kind }),
  caRegenerate: () => invoke<CaView>("ca_regenerate"),
  caExport: (dest: string) => invoke<void>("ca_export", { dest }),

  agentView: (agent: Agent) => invoke<AgentView>("agent_view", { agent }),
  agentEnable: (agent: Agent, mode: ProtectionMode) =>
    invoke<AgentActivation>("agent_enable", { agent, mode }),
  agentDisable: (agent: Agent) => invoke<AgentActivation>("agent_disable", { agent }),
  pacStatus: () => invoke<PacStatus>("pac_status"),
  pacReapply: () => invoke<PacStatus>("pac_reapply"),
  pacRestore: () => invoke<PacStatus>("pac_restore"),

  cliStatus: () => invoke<CliStatus>("cli_status"),
  cliInstall: () => invoke<CliStatus>("cli_install_cmd"),
  cliUninstall: () => invoke<CliStatus>("cli_uninstall_cmd"),

  listRules: () => invoke<Rule[]>("list_rules"),
  setRuleEnabled: (id: string, enabled: boolean) =>
    invoke<Rule[]>("set_rule_enabled", { id, enabled }),
  upsertRule: (rule: Rule) => invoke<Rule[]>("upsert_rule", { rule }),
  deleteRule: (id: string) => invoke<Rule[]>("delete_rule", { id }),
  testRules: (text: string, draft?: Rule) => invoke<RuleTestResult>("test_rules", { text, draft }),
  importRules: (path: string) => invoke<Rule[]>("import_rules", { path }),

  listPersonas: () => invoke<PersonaView>("list_personas"),
  upsertPersona: (persona: Persona) => invoke<PersonaView>("upsert_persona", { persona }),
  deletePersona: (id: string) => invoke<PersonaView>("delete_persona", { id }),
  setPersonaOptions: (activePersona: string | null, substitutionStyle: SubstitutionStyle) =>
    invoke<PersonaView>("set_persona_options", { activePersona, substitutionStyle }),
  previewPersona: (text: string, persona?: Persona) =>
    invoke<RuleTestResult>("preview_persona", { text, persona }),
  exportRules: (dest: string) => invoke<number>("export_rules", { dest }),

  listRequests: (filter: RequestFilter) => invoke<RequestRow[]>("list_requests", { filter }),
  requestDetail: (id: string) => invoke<RequestDetail | null>("request_detail", { id }),
  stats: (sinceHours?: number) => invoke<Stats>("stats", { query: { since_hours: sinceHours ?? null } }),
  usageReport: (hours: number, bucketSecs: number) =>
    invoke<UsageReport>("usage_report", { query: { hours, bucket_secs: bucketSecs } }),
  getPricing: () => invoke<PricingView>("get_pricing"),
  setPricing: (prices: ModelPrice[]) => invoke<PricingView>("set_pricing", { prices }),
  clearLogs: () => invoke<void>("clear_logs"),
  reconcile: () => invoke<string[]>("reconcile"),
};

export function onRequest(cb: (log: RequestLogEvent) => void): Promise<UnlistenFn> {
  return listen<RequestLogEvent>("request:new", (e) => cb(e.payload));
}

export function onProxyStatus(cb: (s: ProxyStatus) => void): Promise<UnlistenFn> {
  return listen<ProxyStatus>("proxy:status", (e) => cb(e.payload));
}

export function onAppNotes(cb: (notes: string[]) => void): Promise<UnlistenFn> {
  return listen<string[]>("app:notes", (e) => cb(e.payload));
}

export const MODE_LABEL: Record<ProtectionMode, string> = {
  config: "A · 写入工具配置",
  system_proxy: "B · 系统代理 (PAC)",
  launcher: "C · pg 启动器",
};

export const AGENT_LABEL: Record<Agent, string> = {
  claude: "Claude Code",
  codex: "Codex",
};
