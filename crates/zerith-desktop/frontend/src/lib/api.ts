// The whole window's contact with Rust. One wrapper per command, so a rename
// in the shell is a type error here instead of a runtime surprise.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Theme = "dark" | "light" | "system";
export type Profile = "economy" | "balanced" | "performance";

export interface Session {
  id: string;
  title: string;
  source: string;
  source_id: string | null;
  source_profile: string | null;
  model: string | null;
  provider: string | null;
  created_at: number;
  updated_at: number;
  archived: boolean;
  /** The folder this conversation works in; null for old/CLI sessions. */
  workspace?: string | null;
}

export interface ProviderView {
  name: string;
  kind: string;
  protocol: string;
  base_url: string;
  default_model: string | null;
  known_models: string[];
  context_limit: number | null;
  timeout_ms: number;
  account_id: string | null;
  has_key: boolean;
  auth: string;
}

export interface ChatMessage {
  id?: number;
  role: string;
  content: string;
  reasoning: string | null;
  tool_name: string | null;
  /** The stored `tool_calls` JSON, when the core sends it. */
  tool_calls?: unknown;
}

export interface Boot {
  root: string;
  db: string;
  theme: Theme;
  profile: Profile;
  reduce_motion: boolean;
  sessions: Session[];
  providers: ProviderView[];
  memory_paused: boolean;
}

export interface Todo {
  id: number;
  session_id: string;
  subject: string;
  description: string;
  status: "pending" | "in_progress" | "completed";
  blocks: number[];
  blocked_by: number[];
  created_at: number;
  updated_at: number;
}

export interface QuestionOption {
  label: string;
  description: string;
}

export interface Question {
  question: string;
  header: string;
  options: QuestionOption[];
  multi_select: boolean;
}

/** One event of a running turn. Mirrors the Rust enum of the same name. */
export type TurnEvent =
  | { type: "delta"; text: string }
  | { type: "reasoning"; text: string }
  | { type: "step"; n: number }
  | { type: "toolCall"; id: string; name: string; args: string }
  | {
      type: "toolResult";
      id: string;
      name: string;
      ok: boolean;
      preview: string;
    }
  | { type: "approval"; id: number; tool: string; summary: string }
  | { type: "usage"; input: number; output: number }
  | { type: "compacted"; before: number; after: number }
  | { type: "autoCompacted"; before: number; after: number }
  | { type: "tasks"; items: Todo[] }
  | {
      type: "subagent";
      id: string;
      name: string;
      state: string;
      summary: string;
      /** The model the child runs on (empty when unknown). */
      model: string;
    }
  | { type: "question"; id: number; questions: Question[] }
  | {
      type: "route";
      accountId: string;
      provider: string;
      model: string;
      attempts: number;
    }
  | { type: "saved"; tool: string; before: number; after: number }
  | {
      type: "done";
      stored: number | null;
      text: string;
      promptTokens: number;
      completionTokens: number;
      savedTokens: number;
      modelUsed: string | null;
    }
  | { type: "failed"; message: string };

/** A provider preset: only the defaults that save typing. */
export interface Preset {
  kind: string;
  label: string;
  base_url: string;
  protocol: string;
  model_hint: string;
  needs_key: boolean;
}

export interface ProviderForm {
  name: string;
  base_url: string;
  protocol: string;
  model: string | null;
  timeout_ms: number;
  key: string | null;
}

export interface ProbeView {
  ok: boolean;
  status: number;
  latency_ms: number;
  models: string[];
  detail: string;
}

export interface Account {
  id: string;
  provider: string;
  auth: string;
  oauth_tool: string | null;
  label: string | null;
  protocol: string | null;
  base_url: string | null;
  default_model: string | null;
  api_key_ref: string | null;
  priority: number;
  enabled: boolean;
  quota_tokens: number | null;
  quota_requests: number | null;
  window_hours: number;
}

export interface AccountForm {
  id: string;
  provider: string;
  priority: number;
  quota_tokens: number | null;
  window_hours: number;
  default_model: string | null;
  enabled: boolean;
  key: string | null;
  /** Catalogue connect only; omitted = keep what the account has. */
  label?: string | null;
  base_url?: string | null;
  protocol?: "openai" | "anthropic" | null;
}

export interface Routing {
  main_model?: string | null;
  subagent_model?: string | null;
  explore_model?: string | null;
  plan_model?: string | null;
  role_models?: Record<string, string>;
  /** With `subagent_model` set, a combo is a delegation profile the chat can pick. */
  combos?: { name: string; models: string[]; subagent_model?: string | null }[];
}

export interface AccountStats {
  account_id: string;
  provider: string;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  saved_tokens: number;
  requests: number;
  rate_limits: number;
  cooldown_until: number;
  last_error: string | null;
  last_model: string | null;
  last_used_at: number | null;
}

export interface UsageSummary {
  accounts: AccountStats[];
  models: {
    model: string;
    provider: string;
    role_kind: string;
    total_tokens: number;
    requests: number;
  }[];
  total_prompt_tokens: number;
  total_completion_tokens: number;
  total_saved_tokens: number;
  total_saved_rtk_tokens: number;
  total_saved_ponytail_tokens: number;
  total_saved_caveman_tokens: number;
  total_requests: number;
  active_account: string | null;
  active_model: string | null;
}

export type FactKind =
  "preference" | "fact" | "decision" | "project" | "instruction" | "entity";

export interface Fact {
  id: number;
  scope: string;
  kind: FactKind;
  text: string;
  source: string | null;
  session_id: string | null;
  confidence: "confirmed" | "inferred";
  created_at: number;
  updated_at: number;
  superseded_by: number | null;
}

export interface Finding {
  severity: "note" | "warn" | "high";
  message: string;
}

export interface InstalledSkill {
  name: string;
  description: string;
  license: string;
  source: string;
  status: "enabled" | "disabled";
  dir: string;
  installed_at: number;
  previous_dir: string | null;
}

export interface SkillView {
  installed: InstalledSkill;
  findings: Finding[];
  scripts: string[];
  inventory: [string, number][];
  body: string;
}

export interface CatalogueEntry {
  repo: string;
  description: string;
  stars: number;
  forks: number;
  updated_at: string;
  license: string;
  url: string;
  note: string;
}

export interface Catalogue {
  fetched_at: number;
  entries: CatalogueEntry[];
}

export interface RemoteSkill {
  /** Folder inside the repository; "" when SKILL.md sits at the root. */
  path: string;
  name: string;
  description: string;
}

export interface RemoteRepo {
  repo: string;
  branch: string;
  fetched_at: number;
  skills: RemoteSkill[];
}

/** A skill folder in the Hermes home (`skills/<categoria>/<nome>`). */
export interface HermesSkill {
  name: string;
  category: string;
  description: string;
  installed: boolean;
  error: string | null;
}

/** What an import from Hermes did, by name. */
export interface SetupImport {
  imported: string[];
  existed: string[];
  failed: string[];
}

/** A configured MCP server. Env/headers come back as keys only (values may be secrets). */
export interface McpServer {
  name: string;
  transport: "stdio" | "http";
  target: string;
  enabled: boolean;
  approved: boolean;
  project: boolean;
  envKeys: string[];
  headerKeys: string[];
  /** `auth = "oauth"`: needs "Entrar" before it works. */
  oauth: boolean;
  /** OAuth tokens are in the vault. */
  connected: boolean;
}

export interface HermesMcp {
  name: string;
  transport: "stdio" | "http";
  target: string;
  status: "importavel" | "nao-encontrado" | "oauth" | "existe" | "invalido";
  enabled: boolean;
  envKeys: string[];
}

export interface PluginView {
  name: string;
  version: string;
  description: string;
  capabilities: string[];
  trusted: boolean;
  enabled: boolean;
  skills: number;
  agents: number;
  /** MCP servers, hooks or a process: off until reviewed. */
  runsCode: boolean;
}

/** One plugin a marketplace offers. */
export interface PluginOffer {
  market: string;
  name: string;
  description: string;
  version: string;
  category: string;
  homepage: string;
}

export interface PluginsView {
  installed: PluginView[];
  offers: PluginOffer[];
  markets: { name: string; source: string }[];
}

export type PluginAction =
  | "install"
  | "enable"
  | "disable"
  | "trust"
  | "untrust"
  | "remove"
  | "market-add"
  | "market-remove";

export interface SaverView {
  ponytail: string;
  caveman: string;
  rtkEnabled: boolean;
}

export interface SaverPatch {
  ponytail?: string;
  caveman?: string;
  rtkEnabled?: boolean;
}

/** billion-context: whether it is on and what the local proxy is doing. */
export interface LongContextView {
  enabled: boolean;
  state: "off" | "missing" | "starting" | "running" | "failed";
  detail: string;
  log: string;
}

export interface StorageView {
  root: string;
  dbPath: string;
  dbBytes: number;
  sessions: number;
  messages: number;
  backupDir: string;
}

export interface ProfileView {
  name: string;
  maxSubagents: number;
  historyWindow: number;
}

export interface Prefs {
  theme: string;
  profile: string;
  reduce_motion: boolean;
}

export interface ParamView {
  name: string;
  kind: string;
  options: string[];
  min: number | null;
  max: number | null;
}

export interface MediaModel {
  id: string;
  name: string;
  description: string;
  priceHint: string;
  params: ParamView[];
  maxReferences: number;
  frameImages: string[];
}

export interface MediaPage {
  total: number;
  models: MediaModel[];
}

export interface MediaKey {
  source: string | null;
  label: string;
  usageUsd: number | null;
  limitUsd: number | null;
}

export interface MediaSlot {
  image: string;
  imageParams: Record<string, unknown>;
  video: string;
  videoParams: Record<string, unknown>;
  outputDir: string;
}

export interface MediaFile {
  path: string;
  mediaType: string;
  bytes: number;
  costUsd: number | null;
}

export interface AgentView {
  name: string;
  description: string;
  model: string | null;
  instructions: string;
  path: string;
  layer: string;
  tools: string[] | null;
  disallowedTools: string[];
  permissionMode: string | null;
  maxTurns: number | null;
  skills: string[];
  isolation: string;
  background: boolean;
  createdByAi: boolean;
  disabled: boolean;
}

export interface AgentForm {
  name: string;
  description: string;
  instructions: string;
  tools: string;
  model: string;
}

export interface SessionRow {
  id: string;
  title: string;
  source: string;
  model: string | null;
  provider: string | null;
  createdAt: number;
  updatedAt: number;
  archived: boolean;
}

export interface ImportSession {
  id: string;
  title: string;
  model: string | null;
  createdAt: number;
  updatedAt: number;
  messageCount: number;
  archived: boolean;
}

export interface SourceView {
  path: string;
  profile: string;
  sessions: ImportSession[];
  totalMessages: number;
  minDate: number | null;
  maxDate: number | null;
}

export interface ReportView {
  sessionsSeen: number;
  sessionsImported: number;
  sessionsSkipped: number;
  sessionsResumed: number;
  messagesImported: number;
  messagesSkipped: number;
  failed: string[];
}

export const api = {
  boot: () => invoke<Boot>("boot"),
  listSessions: (limit = 200) => invoke<Session[]>("list_sessions", { limit }),
  createSession: (title: string, workspace?: string | null) =>
    invoke<Session>("create_session", { title, workspace: workspace ?? null }),
  setSessionWorkspace: (id: string, workspace: string) =>
    invoke<void>("set_session_workspace", { id, workspace }),
  /** Folders used before, newest first; the first is a new chat's default. */
  recentWorkspaces: () => invoke<string[]>("recent_workspaces"),
  /** The OS folder picker; null when cancelled. */
  pickFolder: (start?: string | null) =>
    invoke<string | null>("pick_folder", { start: start ?? null }),
  revealWorkspace: (path: string) => invoke<void>("reveal_workspace", { path }),
  sessionMessages: (id: string, limit = 400) =>
    invoke<ChatMessage[]>("session_messages", { id, limit }),
  /** A copy up to `uptoMessageId` (all when null), same folder. */
  forkSession: (sessionId: string, uptoMessageId: number | null, title: string) =>
    invoke<Session>("session_fork", { sessionId, uptoMessageId, title }),
  setArchived: (id: string, archived: boolean) =>
    invoke<void>("set_archived", { id, archived }),
  exportSession: (id: string) => invoke<string>("export_session", { id }),
  savePrefs: (patch: {
    theme?: Theme;
    profile?: Profile;
    reduce_motion?: boolean;
  }) => invoke<Theme>("save_prefs", { patch }),
  providers: () => invoke<ProviderView[]>("providers"),
  memoryPaused: () => invoke<boolean>("memory_paused"),
  setMemoryPaused: (paused: boolean) =>
    invoke<void>("set_memory_paused", { paused }),
  searchSessions: (query: string) =>
    invoke<string[]>("search_sessions", { query }),
  revealRoot: () => invoke<void>("reveal_root"),
  presets: () => invoke<Preset[]>("presets"),
  providerSave: (form: ProviderForm) =>
    invoke<ProviderView>("provider_save", { form }),
  providerTest: (form: ProviderForm) =>
    invoke<ProbeView>("provider_test", { form }),
  accounts: () => invoke<Account[]>("accounts"),
  accountSave: (form: AccountForm) =>
    invoke<Account[]>("account_save", { form }),
  accountTest: (form: AccountForm) =>
    invoke<ProbeView>("account_test", { form }),
  accountDelete: (id: string) => invoke<Account[]>("account_delete", { id }),
  oauthLogin: (tool: string, account: string) =>
    invoke<string>("oauth_login", { tool, account }),
  oauthCode: (code: string) => invoke<boolean>("oauth_code", { code }),
  accountQuota: (account: string) =>
    invoke<Quota>("account_quota", { account }),
  accountModels: (account: string) =>
    invoke<{ id: string; name: string }[]>("account_models", { account }),
  oauthImport9router: (tool: string, account: string) =>
    invoke<string>("oauth_import_9router", { tool, account }),
  oauthImportPasted: (tool: string, account: string, raw: string) =>
    invoke<string>("oauth_import_pasted", { tool, account, raw }),
  oauthImport: (tool: string, account: string) =>
    invoke<string>("oauth_import", { tool, account }),
  oauthStatus: (account: string) =>
    invoke<{ tool: string; minutes: number } | null>("oauth_status", {
      account,
    }),
  routing: () => invoke<Routing>("routing"),
  routingSave: (routing: Routing) =>
    invoke<Routing>("routing_save", { routing }),
  usage: (hours = 5) => invoke<UsageSummary>("usage", { hours }),
  clearCooldown: (accountId: string) =>
    invoke<void>("clear_cooldown", { accountId }),
  memoryFacts: () => invoke<Fact[]>("memory_facts"),
  memorySearch: (query: string) => invoke<number[]>("memory_search", { query }),
  memoryUpdate: (id: number, text: string) =>
    invoke<void>("memory_update", { id, text }),
  memoryForget: (id: number) => invoke<void>("memory_forget", { id }),
  skillsList: () => invoke<SkillView[]>("skills_list"),
  skillsCatalog: (force = false) =>
    invoke<Catalogue>("skills_catalog", { force }),
  skillsPlugins: () => invoke<PluginsView>("skills_plugins"),
  pluginAction: (action: PluginAction, target: string) =>
    invoke<PluginsView>("plugin_action", { action, target }),
  skillInstall: (from: string) =>
    invoke<SkillView[]>("skill_install", { from }),
  skillToggle: (name: string, enabled: boolean) =>
    invoke<SkillView[]>("skill_toggle", { name, enabled }),
  skillRemove: (name: string) => invoke<SkillView[]>("skill_remove", { name }),
  skillsRemote: (repo: string, force = false) =>
    invoke<RemoteRepo>("skills_remote", { repo, force }),
  skillInstallRemote: (repo: string, branch: string, path: string) =>
    invoke<SkillView[]>("skill_install_remote", { repo, branch, path }),
  hermesSkills: () => invoke<HermesSkill[]>("hermes_skills"),
  hermesSkillsImport: (names: string[]) =>
    invoke<SetupImport & { skills: SkillView[] }>("hermes_skills_import", { names }),
  mcpList: () => invoke<McpServer[]>("mcp_list"),
  mcpAdd: (form: { name: string; command: string; url: string; env: string; headers: string }) =>
    invoke<McpServer[]>("mcp_add", form),
  mcpRemove: (name: string) => invoke<McpServer[]>("mcp_remove", { name }),
  mcpToggle: (name: string, enabled: boolean) => invoke<McpServer[]>("mcp_toggle", { name, enabled }),
  mcpApprove: (name: string) => invoke<McpServer[]>("mcp_approve", { name }),
  mcpTest: (name: string) => invoke<{ name: string; description: string }[]>("mcp_test", { name }),
  mcpHermesPreview: () => invoke<HermesMcp[]>("mcp_hermes_preview"),
  mcpHermesImport: (names: string[]) => invoke<SetupImport>("mcp_hermes_import", { names }),
  mcpOauthLogin: (name: string) => invoke<McpServer[]>("mcp_oauth_login", { name }),
  mcpOauthStatus: (name: string) =>
    invoke<{ connected: boolean; expiresAt: number | null }>("mcp_oauth_status", { name }),
  mcpOauthLogout: (name: string) => invoke<McpServer[]>("mcp_oauth_logout", { name }),
  workspaceFiles: (root?: string) =>
    invoke<string[]>("workspace_files", { root: root ?? null }),
  /** A local image from the transcript (`![](C:/x.png)`) as a data URL. */
  readImage: (path: string) => invoke<string>("read_image", { path }),
  openImage: (path: string) => invoke<void>("open_image", { path }),
  /** An http(s)/mailto link, in the system browser (the webview opens none). */
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  /** An attached image (data URL) saved under `<root>/attachments`; returns its path. */
  saveAttachment: (data: string) => invoke<string>("save_attachment", { data }),
  checkpoints: (sessionId: string) =>
    invoke<number[]>("checkpoints", { sessionId }),
  rewind: (sessionId: string) =>
    invoke<{ turn: number; text: string }>("rewind", { sessionId }),
  compact: (sessionId: string, model: string) =>
    invoke<number>("compact", { sessionId, model }),
  tokenSaver: () => invoke<SaverView>("token_saver"),
  setTokenSaver: (patch: SaverPatch) =>
    invoke<SaverView>("set_token_saver", { patch }),
  longContext: () => invoke<LongContextView>("long_context"),
  setLongContext: (enabled: boolean) =>
    invoke<LongContextView>("set_long_context", { enabled }),
  installBillionContext: () =>
    invoke<LongContextView>("install_billion_context"),
  storage: () => invoke<StorageView>("storage"),
  backup: () => invoke<string>("backup"),
  profileLimits: () => invoke<ProfileView[]>("profile_limits"),
  mediaModels: (kind: string, query = "", offset = 0, limit = 200) =>
    invoke<MediaPage>("media_models", { kind, query, offset, limit }),
  mediaKey: (check = false) => invoke<MediaKey>("media_key", { check }),
  mediaSaveKey: (key: string) => invoke<void>("media_save_key", { key }),
  mediaSettings: () => invoke<MediaSlot>("media_settings"),
  mediaSaveSlot: (edit: {
    kind: string;
    model: string;
    params: Record<string, unknown>;
  }) => invoke<void>("media_save_slot", { edit }),
  mediaGenerate: (req: {
    kind: string;
    model: string;
    prompt: string;
    params?: Record<string, unknown>;
    references?: string[];
    firstFrame?: string | null;
    lastFrame?: string | null;
  }) => invoke<MediaFile[]>("media_generate", { req }),
  mediaOpen: (path: string) => invoke<void>("media_open", { path }),
  mediaOpenDir: () => invoke<void>("media_open_dir"),
  agentsList: () => invoke<AgentView[]>("agents_list"),
  agentCreate: (form: AgentForm) =>
    invoke<AgentView[]>("agent_create", { form }),
  agentToggle: (name: string, disabled: boolean) =>
    invoke<AgentView[]>("agent_toggle", { name, disabled }),
  agentReveal: () => invoke<void>("agent_reveal"),
  sessionsHistory: (includeArchived = false) =>
    invoke<SessionRow[]>("sessions_history", { includeArchived }),
  hermesDetect: () => invoke<SourceView[]>("hermes_detect"),
  hermesPreview: (path: string) =>
    invoke<SourceView>("hermes_preview", { path }),
  hermesImport: (
    path: string,
    includeArchived = true,
    onlyIds: string[] | null = null,
  ) => invoke<ReportView>("hermes_import", { path, includeArchived, onlyIds }),
  sendTurn: (args: {
    sessionId: string;
    model: string;
    prompt: string;
    mode: string;
    agent?: string | null;
    history?: number;
  }) =>
    invoke<void>("send_turn", {
      args: {
        session_id: args.sessionId,
        model: args.model,
        prompt: args.prompt,
        mode: args.mode,
        agent: args.agent ?? null,
        // 0 = the profile's history window (the core decides).
        history: args.history ?? 0,
      },
    }),
  cancelTurn: (sessionId: string) => invoke<void>("cancel_turn", { sessionId }),
  answerApproval: (
    sessionId: string,
    id: number,
    decision: "once" | "session" | "deny",
  ) => invoke<boolean>("answer_approval", { sessionId, id, decision }),
  answerQuestion: (sessionId: string, id: number, answers: string[]) =>
    invoke<boolean>("answer_question", { sessionId, id, answers }),
};

/** Turn events of every conversation, so a turn running in a chat that is not
 * open keeps its state (and its approval card) for when it is opened again. */
export async function onTurnEvent(
  handler: (sessionId: string, ev: TurnEvent) => void,
): Promise<UnlistenFn> {
  return listen<[string, TurnEvent]>("turn:event", (e) => handler(e.payload[0], e.payload[1]));
}

export function onTurnDone(
  handler: (sessionId: string) => void,
): Promise<UnlistenFn> {
  return listen<string>("turn:done", (e) => handler(e.payload));
}

/** The authorize link of the login in progress (shown so it can be copied). */
/** Subscription windows, as the plan reports them (Claude Code). */
export type QuotaWindow = {
  utilization: number;
  resets_at: string | null;
  resets_at_epoch: number | null;
};
/** One named limit, one bar on the Cotas screen. `group` = "Janelas" | "Modelos". */
export type QuotaBucket = { group: string; label: string; window: QuotaWindow };
export type Quota = {
  email: string | null;
  plan: string | null;
  five_hour: QuotaWindow | null;
  seven_day: QuotaWindow | null;
  buckets: QuotaBucket[];
};

export function onOAuthUrl(
  handler: (url: string) => void,
): Promise<UnlistenFn> {
  return listen<string>("oauth:url", (e) => handler(e.payload));
}

/** One thing the background review learned after a turn (`turn:learned`). */
export interface LearnedItem {
  kind: "memória" | "skill";
  op: "add" | "update" | "create" | "patch";
  text: string;
  id: number | null;
  name: string | null;
  /** What undo restores; null = undo deletes. */
  previous: string | null;
}
export interface LearningView {
  enabled: boolean;
  model: string;
}

export const learnApi = {
  get: () => invoke<LearningView>("learning"),
  set: (patch: { enabled?: boolean; model?: string }) =>
    invoke<LearningView>("set_learning", { patch }),
  undo: (item: LearnedItem) => invoke<void>("learn_undo", { item }),
};

export function onTurnLearned(
  handler: (sessionId: string, items: LearnedItem[]) => void,
): Promise<UnlistenFn> {
  return listen<[string, LearnedItem[]]>("turn:learned", (e) =>
    handler(e.payload[0], e.payload[1]),
  );
}

/** One file an answer changed (`turn_changes`). `diff` = unified hunks. */
export interface FileChange {
  path: string;
  rel: string;
  status: "criado" | "alterado" | "apagado";
  added: number;
  removed: number;
  diff: string | null;
  note: string | null;
  truncated: boolean;
  /** The file differs from what the answer left; null = unknown. */
  changedSince: boolean | null;
}
export interface UndoView {
  /** False = refused because `conflicts` changed after the answer. */
  done: boolean;
  conflicts: string[];
  filesRestored: number;
  filesRemoved: number;
  errors: string[];
}

export const changesApi = {
  list: (sessionId: string, messageId: number) =>
    invoke<FileChange[]>("turn_changes", { sessionId, messageId }),
  undo: (sessionId: string, messageId: number, force = false) =>
    invoke<UndoView>("turn_undo", { sessionId, messageId, force }),
};

/** What `update_check` found on GitHub's latest release. */
export interface UpdateInfo {
  current: string;
  latest: string | null;
  /** True only when there is a newer installer to run. */
  newer: boolean;
  notes: string;
  /** One pt-BR line for the window. */
  message: string;
  size: number | null;
}

/** A background shell job (`shell run_in_background`) still running. */
export interface BackgroundJob {
  id: string;
  command: string;
  /** The conversation whose turn started it (a subagent's own session for a child). */
  session: string | null;
  /** Seconds since it started. */
  secs: number;
}

export const jobsApi = {
  list: () => invoke<BackgroundJob[]>("background_jobs"),
  stop: (id: string) => invoke<string>("background_job_stop", { id }),
};

export const updateApi = {
  check: () => invoke<UpdateInfo>("update_check"),
  /** Downloads, verifies SHA-256 + Ed25519 signature, starts the installer and quits. */
  install: () => invoke<void>("update_install"),
};
