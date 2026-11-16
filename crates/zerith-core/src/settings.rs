//! Layered settings.
//!
//! Precedence, low to high: built-in defaults < user `<root>/settings.toml` <
//! project `<project>/.zerith/settings.toml` < project-local
//! `<project>/.zerith/settings.local.toml` < environment variables.
//!
//! Layers are merged as TOML trees before they are typed: tables merge key by
//! key, arrays concatenate (permission rules and hooks add up across layers),
//! scalars are replaced. A layer that fails to parse or type-check is skipped
//! whole and reported with file and line, so a typo in one file never stops
//! the app — it starts with the layers below.
//!
//! API keys never live here. A provider names its key (`api_key_ref`) and the
//! key itself is read from the OS keyring (see `secrets`).

use crate::Paths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml::{Table, Value};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Name of the provider to use; matches `providers[].name` or a built-in
    /// kind (`openai`, `anthropic`, `deepseek`, `gemini`, `openrouter`, `ollama`,
    /// `lmstudio`). Env: `ZERITH_PROVIDER`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Endpoint override for the active provider. Env: `ZERITH_BASE_URL`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Default model. Env: `ZERITH_MODEL`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Owner of the memory scope. Env: `ZERITH_USER`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    pub providers: Vec<ProviderEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<AccountEntry>,
    #[serde(default)]
    pub routing: RoutingSettings,
    #[serde(default)]
    pub token_saver: TokenSaverSettings,
    pub permissions: Permissions,
    pub hooks: Hooks,
    pub ui: Ui,
    pub profile: Profile,
    pub memory: MemorySettings,
}

fn default_true() -> bool {
    true
}

fn default_window_hours() -> u32 {
    5
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountEntry {
    pub id: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<WireProtocol>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Name of the keyring entry holding the key. Never the key itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_ref: Option<String>,
    pub priority: i32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota_requests: Option<u64>,
    #[serde(default = "default_window_hours")]
    pub window_hours: u32,
}

impl Default for AccountEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            provider: String::new(),
            label: None,
            protocol: None,
            base_url: None,
            default_model: None,
            api_key_ref: None,
            priority: 0,
            enabled: true,
            quota_tokens: None,
            quota_requests: None,
            window_hours: 5,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComboEntry {
    pub name: String,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutingSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explore_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_model: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub role_models: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub combos: Vec<ComboEntry>,
}

fn non_empty(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|s| !s.is_empty())
}

impl RoutingSettings {
    /// Resolves which model a subagent should use.
    ///
    /// Precedence: `explicit` -> `def_model` -> `role_models[agent_name]` ->
    /// `explore_model` / `plan_model` -> `subagent_model` -> `parent_model`.
    /// Empty or whitespace-only values are treated as absent.
    pub fn resolve_subagent_model(
        &self,
        agent_name: &str,
        explicit: Option<&str>,
        def_model: Option<&str>,
        parent_model: &str,
    ) -> String {
        let role_specific = match agent_name {
            "explore" => self.explore_model.as_deref(),
            "plan" => self.plan_model.as_deref(),
            _ => None,
        };
        non_empty(explicit)
            .or_else(|| non_empty(def_model))
            .or_else(|| non_empty(self.role_models.get(agent_name).map(String::as_str)))
            .or_else(|| non_empty(role_specific))
            .or_else(|| non_empty(self.subagent_model.as_deref()))
            .unwrap_or(parent_model)
            .to_string()
    }

    /// The last combo with this name: a project can redefine a combo the user
    /// layer declared.
    pub fn combo(&self, name: &str) -> Option<&ComboEntry> {
        self.combos.iter().rev().find(|c| c.name == name)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PonytailLevel {
    #[default]
    Off,
    Lite,
    Full,
    Ultra,
}

impl PonytailLevel {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "lite" => Some(Self::Lite),
            "full" => Some(Self::Full),
            "ultra" => Some(Self::Ultra),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Lite => "lite",
            Self::Full => "full",
            Self::Ultra => "ultra",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CavemanLevel {
    #[default]
    Off,
    Lite,
    Full,
    Ultra,
    Wenyan,
}

impl CavemanLevel {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "lite" => Some(Self::Lite),
            "full" => Some(Self::Full),
            "ultra" => Some(Self::Ultra),
            "wenyan" => Some(Self::Wenyan),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Lite => "lite",
            Self::Full => "full",
            Self::Ultra => "ultra",
            Self::Wenyan => "wenyan",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TokenSaverSettings {
    pub ponytail: PonytailLevel,
    pub caveman: CavemanLevel,
    #[serde(default = "default_true")]
    pub rtk_enabled: bool,
}

impl Default for TokenSaverSettings {
    fn default() -> Self {
        Self {
            ponytail: PonytailLevel::Off,
            caveman: CavemanLevel::Off,
            rtk_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<WireProtocol>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_limit: Option<u32>,
    /// Name of the keyring entry holding the key. Never the key itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireProtocol {
    OpenAi,
    Anthropic,
}

/// Outcome of a permission rule or a hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Permissions {
    /// Sandbox preset name (`read-only`, `workspace-write`,
    /// `danger-full-access`). A permission mode name (`plan`, `acceptEdits`,
    /// ...) is also accepted here, as shorthand for `default_mode` with the
    /// default preset; the two name sets are disjoint. Env: `ZERITH_SANDBOX`.
    pub mode: String,
    /// How eagerly the agent acts without asking: `default`, `acceptEdits`,
    /// `plan`, `dontAsk`, `bypassPermissions`. Absent: derived from the
    /// preset. Env: `ZERITH_MODE`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_mode: Option<String>,
    /// Rules as `Tool` or `Tool(specifier)`; `*` in the specifier matches
    /// anything. See `docs/permissions.md` for the full syntax.
    pub allow: Vec<String>,
    pub ask: Vec<String>,
    pub deny: Vec<String>,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            mode: "workspace-write".into(),
            default_mode: None,
            allow: Vec::new(),
            ask: Vec::new(),
            deny: Vec::new(),
        }
    }
}

impl Permissions {
    /// The first matching rule, checked deny → ask → allow, so a deny in any
    /// layer cannot be undone by an allow in another. `None`: no rule matched
    /// and the caller falls back to the sandbox preset.
    pub fn decide(&self, tool: &str, arg: &str) -> Option<Decision> {
        let hit = |rules: &[String]| rules.iter().any(|r| rule_matches(r, tool, arg));
        if hit(&self.deny) {
            Some(Decision::Deny)
        } else if hit(&self.ask) {
            Some(Decision::Ask)
        } else if hit(&self.allow) {
            Some(Decision::Allow)
        } else {
            None
        }
    }
}

/// `Tool` matches every call of that tool; `Tool(pattern)` matches when the
/// argument (path, command, URL) matches the glob.
pub fn rule_matches(rule: &str, tool: &str, arg: &str) -> bool {
    let rule = rule.trim();
    match rule.split_once('(') {
        Some((name, rest)) => {
            let pattern = rest.strip_suffix(')').unwrap_or(rest);
            name.trim() == tool && glob_match(pattern, arg)
        }
        None => rule == tool,
    }
}

/// `*` matches any run of characters; everything else is literal.
// ponytail: no `?`, classes or `**` semantics; add a glob crate if rules need them.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookSpec {
    /// `|`-separated entries, each `tool` (a `*` glob) or `tool(specifier)`
    /// (specifier: a `*` glob over the command / path / URL, as in permission
    /// rules). Empty or absent: every tool. Only used by the tool events
    /// (`PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionRequest`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub command: String,
    #[serde(default = "default_hook_timeout")]
    pub timeout_ms: u64,
}

fn default_hook_timeout() -> u64 {
    60_000
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Hooks {
    pub session_start: Vec<HookSpec>,
    pub user_prompt_submit: Vec<HookSpec>,
    pub pre_tool_use: Vec<HookSpec>,
    pub post_tool_use: Vec<HookSpec>,
    pub post_tool_use_failure: Vec<HookSpec>,
    pub permission_request: Vec<HookSpec>,
    pub stop: Vec<HookSpec>,
    pub pre_compact: Vec<HookSpec>,
    pub session_end: Vec<HookSpec>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Dark,
    Light,
    #[default]
    System,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    pub theme: Theme,
    pub reduced_motion: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Economy,
    #[default]
    Balanced,
    Performance,
}

/// What a profile means in numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileLimits {
    pub max_subagents: usize,
    /// Messages replayed to the model per turn.
    pub history_window: usize,
    /// Entries kept by in-memory caches (models list, recall results).
    pub cache_entries: usize,
}

impl Profile {
    pub fn limits(self) -> ProfileLimits {
        match self {
            Profile::Economy => ProfileLimits {
                max_subagents: 1,
                history_window: 20,
                cache_entries: 64,
            },
            Profile::Balanced => ProfileLimits {
                max_subagents: 2,
                history_window: 40,
                cache_entries: 256,
            },
            Profile::Performance => ProfileLimits {
                max_subagents: 8,
                history_window: 80,
                cache_entries: 1024,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemorySettings {
    pub paused: bool,
    pub budget_chars: usize,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            paused: false,
            budget_chars: 2000,
        }
    }
}

/// Environment variables read as the top layer, and the key each sets.
pub const ENV_KEYS: &[(&str, &str)] = &[
    ("ZERITH_PROVIDER", "provider"),
    ("ZERITH_BASE_URL", "base_url"),
    ("ZERITH_MODEL", "model"),
    ("ZERITH_USER", "user"),
    ("ZERITH_SANDBOX", "permissions.mode"),
    ("ZERITH_MODE", "permissions.default_mode"),
    ("ZERITH_PROFILE", "profile"),
    ("ZERITH_SUBAGENT_MODEL", "routing.subagent_model"),
    ("ZERITH_PONYTAIL", "token_saver.ponytail"),
    ("ZERITH_CAVEMAN", "token_saver.caveman"),
];

/// Merged settings plus where each value came from and what was skipped.
#[derive(Debug, Clone, Default)]
pub struct LoadedSettings {
    pub settings: Settings,
    /// Layer label (`default`, `user`, `project`, `local`, `env`) per dotted
    /// key; array items are keyed `path[i]`.
    pub sources: BTreeMap<String, String>,
    /// One line per skipped layer, naming the file and line.
    pub errors: Vec<String>,
    merged: Table,
}

impl LoadedSettings {
    /// Every leaf as `(key, value, layer)`, secret-looking values masked.
    pub fn entries(&self) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        flatten("", &Value::Table(self.merged.clone()), &mut out);
        out.into_iter()
            .map(|(k, v)| {
                let src = self.source_of(&k);
                let leaf = k.rsplit(['.', '[']).next().unwrap_or(&k);
                let secret = ["api_key", "token", "password", "secret"]
                    .iter()
                    .any(|s| leaf.ends_with(s));
                (k, if secret { "****".into() } else { v }, src)
            })
            .collect()
    }

    fn source_of(&self, key: &str) -> String {
        let mut k = key;
        loop {
            if let Some(s) = self.sources.get(k) {
                return s.clone();
            }
            match k.rfind(['.', '[']) {
                Some(i) => k = &k[..i],
                None => return "default".into(),
            }
        }
    }
}

fn flatten(prefix: &str, v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::Table(t) => {
            for (k, v) in t {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&p, v, out);
            }
        }
        Value::Array(a) if !a.is_empty() => {
            for (i, v) in a.iter().enumerate() {
                flatten(&format!("{prefix}[{i}]"), v, out);
            }
        }
        other => out.push((prefix.to_string(), other.to_string())),
    }
}

impl Settings {
    /// Reads every layer from disk and the process environment.
    pub fn load(paths: &Paths, project_dir: Option<&Path>) -> LoadedSettings {
        Self::load_with_env(paths, project_dir, |k| std::env::var(k).ok())
    }

    /// Same as `load`, with the environment injected (tests, hosts that
    /// sandbox their env).
    pub fn load_with_env(
        paths: &Paths,
        project_dir: Option<&Path>,
        env: impl Fn(&str) -> Option<String>,
    ) -> LoadedSettings {
        let mut out = LoadedSettings::default();
        let mut merged = match Value::try_from(Settings::default()) {
            Ok(Value::Table(t)) => t,
            _ => Table::new(),
        };

        let mut files: Vec<(&str, PathBuf)> = vec![("user", paths.settings())];
        if let Some(dir) = project_dir {
            files.push(("project", dir.join(".zerith").join("settings.toml")));
            files.push(("local", dir.join(".zerith").join("settings.local.toml")));
        }
        for (label, path) in files {
            match read_layer(&path) {
                Ok(Some(t)) => merge(&mut merged, t, label, "", &mut out.sources),
                Ok(None) => {}
                Err(e) => out.errors.push(e),
            }
        }

        let env_layer = env_table(&env);
        match Value::Table(env_layer.clone()).try_into::<Settings>() {
            Ok(_) => merge(&mut merged, env_layer, "env", "", &mut out.sources),
            Err(e) => out
                .errors
                .push(format!("variáveis de ambiente ignoradas: {}", e.message())),
        }

        match Value::Table(merged.clone()).try_into::<Settings>() {
            Ok(s) => out.settings = s,
            Err(e) => out
                .errors
                .push(format!("configuração combinada inválida: {}", e.message())),
        }
        out.merged = merged;
        out
    }

    /// The last provider entry with this name: a project can redefine a
    /// provider the user layer declared.
    pub fn provider_entry(&self, name: &str) -> Option<&ProviderEntry> {
        self.providers.iter().rev().find(|p| p.name == name)
    }

    /// The last account entry with this id: a project can redefine an
    /// account the user layer declared.
    pub fn account(&self, id: &str) -> Option<&AccountEntry> {
        self.accounts.iter().rev().find(|a| a.id == id)
    }

    /// Edits the user layer only and writes it back.
    ///
    /// Reads the user file alone (not the merged view) so project or env
    /// values never leak into it. A corrupt user file is not overwritten:
    /// the user would lose whatever they wrote.
    pub fn save_user(paths: &Paths, edit: impl FnOnce(&mut Settings)) -> crate::Result<()> {
        let path = paths.settings();
        let mut current = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str::<Settings>(&text)
                .map_err(|e| crate::Error::Other(describe(&path, &text, &e)))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => return Err(e.into()),
        };
        edit(&mut current);
        let text = toml::to_string_pretty(&current)
            .map_err(|e| crate::Error::Other(format!("falha ao serializar: {e}")))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write then rename, so a crash mid-write leaves the old file intact.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

fn read_layer(path: &Path) -> Result<Option<Table>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: não foi possível ler: {e}", path.display())),
    };
    // Typed first: it reports both syntax and type errors with a span.
    toml::from_str::<Settings>(&text).map_err(|e| describe(path, &text, &e))?;
    text.parse::<Table>()
        .map(Some)
        .map_err(|e| describe(path, &text, &e))
}

fn describe(path: &Path, text: &str, e: &toml::de::Error) -> String {
    let line = e
        .span()
        .map(|s| text[..s.start.min(text.len())].matches('\n').count() + 1)
        .unwrap_or(1);
    format!(
        "{}:{line}: configuração inválida, camada ignorada: {}",
        path.display(),
        e.message()
    )
}

fn env_table(env: &impl Fn(&str) -> Option<String>) -> Table {
    let mut t = Table::new();
    for (var, key) in ENV_KEYS {
        let Some(v) = env(var).filter(|v| !v.trim().is_empty()) else {
            continue;
        };
        match key.split_once('.') {
            Some((outer, inner)) => {
                let entry = t
                    .entry(outer.to_string())
                    .or_insert_with(|| Value::Table(Table::new()));
                if let Value::Table(inner_t) = entry {
                    inner_t.insert(inner.to_string(), Value::String(v));
                }
            }
            None => {
                t.insert(key.to_string(), Value::String(v));
            }
        }
    }
    t
}

fn merge(
    base: &mut Table,
    over: Table,
    layer: &str,
    prefix: &str,
    sources: &mut BTreeMap<String, String>,
) {
    for (k, v) in over {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match (base.get_mut(&k), v) {
            (Some(Value::Table(b)), Value::Table(o)) => merge(b, o, layer, &path, sources),
            (Some(Value::Array(b)), Value::Array(o)) => {
                for item in o {
                    sources.insert(format!("{path}[{}]", b.len()), layer.into());
                    b.push(item);
                }
            }
            (_, v) => {
                let (dot, bracket) = (format!("{path}."), format!("{path}["));
                sources.retain(|p, _| !(p.starts_with(&dot) || p.starts_with(&bracket)));
                sources.insert(path, layer.into());
                base.insert(k, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new().tempdir_in(root).unwrap()
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn layers_override_scalars_and_concatenate_arrays() {
        let dir = scratch();
        let paths = Paths::with_root(dir.path().join("data"));
        let project = dir.path().join("proj");
        write(
            &paths.settings(),
            "model = \"a\"\nprofile = \"economy\"\n[permissions]\nallow = [\"read_file\"]\n[ui]\ntheme = \"dark\"\n",
        );
        write(
            &project.join(".zerith/settings.toml"),
            "model = \"b\"\n[permissions]\ndeny = [\"write_file(*.env)\"]\nallow = [\"list_dir\"]\n",
        );
        write(
            &project.join(".zerith/settings.local.toml"),
            "model = \"c\"\n[[hooks.PreToolUse]]\nmatcher = \"shell\"\ncommand = \"x\"\n",
        );
        let env = |k: &str| (k == "ZERITH_SANDBOX").then(|| "read-only".to_string());
        let l = Settings::load_with_env(&paths, Some(&project), env);
        assert!(l.errors.is_empty(), "{:?}", l.errors);
        let s = &l.settings;
        assert_eq!(s.model.as_deref(), Some("c"));
        assert_eq!(s.profile, Profile::Economy);
        assert_eq!(s.ui.theme, Theme::Dark);
        assert_eq!(s.permissions.allow, vec!["read_file", "list_dir"]);
        assert_eq!(s.permissions.mode, "read-only");
        assert_eq!(s.hooks.pre_tool_use[0].timeout_ms, 60_000);
        assert_eq!(
            s.permissions.decide("write_file", "a/.env"),
            Some(Decision::Deny)
        );
        assert_eq!(s.permissions.decide("list_dir", "x"), Some(Decision::Allow));
        assert_eq!(s.permissions.decide("shell", "x"), None);

        let entries = l.entries();
        let src = |k: &str| entries.iter().find(|e| e.0 == k).map(|e| e.2.clone());
        assert_eq!(src("model").as_deref(), Some("local"));
        assert_eq!(src("permissions.allow[0]").as_deref(), Some("user"));
        assert_eq!(src("permissions.allow[1]").as_deref(), Some("project"));
        assert_eq!(src("permissions.mode").as_deref(), Some("env"));
        assert_eq!(src("memory.budget_chars").as_deref(), Some("default"));
    }

    #[test]
    fn a_corrupt_layer_is_reported_with_file_and_line_and_skipped() {
        let dir = scratch();
        let paths = Paths::with_root(dir.path().join("data"));
        let project = dir.path().join("proj");
        write(&paths.settings(), "model = \"ok\"\n");
        write(
            &project.join(".zerith/settings.toml"),
            "model = \"bad\"\n\n[ui]\ntheme = \"purple\"\n",
        );
        let l = Settings::load_with_env(&paths, Some(&project), |_| None);
        assert_eq!(l.errors.len(), 1);
        assert!(l.errors[0].contains("settings.toml:4:"), "{}", l.errors[0]);
        assert_eq!(l.settings.model.as_deref(), Some("ok"));
        assert_eq!(l.settings.ui.theme, Theme::System);

        write(&paths.settings(), "model = \n");
        let l = Settings::load_with_env(&paths, None, |_| None);
        assert!(l.errors[0].contains(":1:"), "{}", l.errors[0]);
        assert_eq!(l.settings, Settings::default());
    }

    #[test]
    fn save_user_touches_only_the_user_layer_and_refuses_corrupt_files() {
        let dir = scratch();
        let paths = Paths::with_root(dir.path().join("data"));
        Settings::save_user(&paths, |s| s.ui.theme = Theme::Light).unwrap();
        Settings::save_user(&paths, |s| s.ui.reduced_motion = true).unwrap();
        let l = Settings::load_with_env(&paths, None, |k| {
            (k == "ZERITH_MODEL").then(|| "env-model".to_string())
        });
        assert_eq!(l.settings.ui.theme, Theme::Light);
        assert!(l.settings.ui.reduced_motion);
        let text = std::fs::read_to_string(paths.settings()).unwrap();
        assert!(!text.contains("env-model"));

        write(&paths.settings(), "[ui\n");
        assert!(Settings::save_user(&paths, |s| s.ui.theme = Theme::Dark).is_err());
        assert_eq!(std::fs::read_to_string(paths.settings()).unwrap(), "[ui\n");
    }

    #[test]
    fn globs_and_rules() {
        assert!(glob_match("*", ""));
        assert!(glob_match("git *", "git status"));
        assert!(glob_match("*.env", "a/b.env"));
        assert!(glob_match("a*b*c", "aXbYc"));
        assert!(!glob_match("a*b", "ab-c"));
        assert!(rule_matches("shell", "shell", "anything"));
        assert!(rule_matches("shell(git *)", "shell", "git log"));
        assert!(!rule_matches("shell(git *)", "shell", "rm -rf /"));
        assert_eq!(Profile::Performance.limits().max_subagents, 8);
    }

    #[test]
    fn accounts_routing_combos_and_token_saver_parse_and_merge() {
        let dir = scratch();
        let paths = Paths::with_root(dir.path().join("data"));
        let project = dir.path().join("proj");

        write(
            &paths.settings(),
            "\
[[accounts]]
id = \"openai-work\"
provider = \"openai\"
label = \"Work\"
protocol = \"openai\"
base_url = \"https://api.openai.com/v1\"
default_model = \"gpt-4o\"
api_key_ref = \"openai_work\"
priority = 10
quota_tokens = 100000
quota_requests = 500

[routing]
main_model = \"gpt-4o\"
subagent_model = \"gpt-4o-mini\"
explore_model = \"haiku\"

[routing.role_models]
reviewer = \"sonnet\"

[[routing.combos]]
name = \"fast\"
models = [\"gpt-4o-mini\", \"haiku\"]

[token_saver]
ponytail = \"lite\"
caveman = \"full\"
rtk_enabled = false
",
        );
        write(
            &project.join(".zerith/settings.toml"),
            "\
[[accounts]]
id = \"anthropic-dev\"
provider = \"anthropic\"
enabled = false
window_hours = 24

[routing]
plan_model = \"opus\"

[routing.role_models]
coder = \"sonnet-4\"

[[routing.combos]]
name = \"smart\"
models = [\"opus\", \"gpt-4o\"]

[token_saver]
ponytail = \"ultra\"
",
        );
        write(
            &project.join(".zerith/settings.local.toml"),
            "\
[[accounts]]
id = \"openai-work\"
provider = \"openai\"
priority = 20

[[routing.combos]]
name = \"fast\"
models = [\"gemini-flash\"]
",
        );

        let l = Settings::load_with_env(&paths, Some(&project), |_| None);
        assert!(l.errors.is_empty(), "{:?}", l.errors);
        let s = &l.settings;

        assert_eq!(s.accounts.len(), 3);
        let a0 = &s.accounts[0];
        assert_eq!(a0.id, "openai-work");
        assert_eq!(a0.provider, "openai");
        assert_eq!(a0.label.as_deref(), Some("Work"));
        assert_eq!(a0.protocol, Some(WireProtocol::OpenAi));
        assert_eq!(a0.base_url.as_deref(), Some("https://api.openai.com/v1"));
        assert_eq!(a0.default_model.as_deref(), Some("gpt-4o"));
        assert_eq!(a0.api_key_ref.as_deref(), Some("openai_work"));
        assert_eq!(a0.priority, 10);
        assert!(a0.enabled);
        assert_eq!(a0.quota_tokens, Some(100_000));
        assert_eq!(a0.quota_requests, Some(500));
        assert_eq!(a0.window_hours, 5);

        let a1 = &s.accounts[1];
        assert_eq!(a1.id, "anthropic-dev");
        assert!(!a1.enabled);
        assert_eq!(a1.window_hours, 24);
        assert_eq!(a1.priority, 0);
        assert_eq!(a1.label, None);

        assert_eq!(s.account("openai-work").map(|a| a.priority), Some(20));
        assert_eq!(s.account("anthropic-dev").map(|a| a.enabled), Some(false));
        assert!(s.account("missing").is_none());

        assert_eq!(s.routing.main_model.as_deref(), Some("gpt-4o"));
        assert_eq!(s.routing.subagent_model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(s.routing.explore_model.as_deref(), Some("haiku"));
        assert_eq!(s.routing.plan_model.as_deref(), Some("opus"));
        assert_eq!(
            s.routing.role_models.get("reviewer").map(String::as_str),
            Some("sonnet")
        );
        assert_eq!(
            s.routing.role_models.get("coder").map(String::as_str),
            Some("sonnet-4")
        );
        assert_eq!(s.routing.combos.len(), 3);
        assert_eq!(
            s.routing.combo("fast").unwrap().models,
            vec!["gemini-flash"]
        );
        assert_eq!(
            s.routing.combo("smart").unwrap().models,
            vec!["opus", "gpt-4o"]
        );
        assert!(s.routing.combo("missing").is_none());

        assert_eq!(s.token_saver.ponytail, PonytailLevel::Ultra);
        assert_eq!(s.token_saver.caveman, CavemanLevel::Full);
        assert!(!s.token_saver.rtk_enabled);

        let defaults = TokenSaverSettings::default();
        assert_eq!(defaults.ponytail, PonytailLevel::Off);
        assert_eq!(defaults.caveman, CavemanLevel::Off);
        assert!(defaults.rtk_enabled);

        let entries = l.entries();
        let src = |k: &str| entries.iter().find(|e| e.0 == k).map(|e| e.2.clone());
        assert_eq!(src("accounts[0].id").as_deref(), Some("user"));
        assert_eq!(src("accounts[1].id").as_deref(), Some("project"));
        assert_eq!(src("accounts[2].id").as_deref(), Some("local"));
        assert_eq!(src("routing.combos[0].name").as_deref(), Some("user"));
        assert_eq!(src("routing.combos[1].name").as_deref(), Some("project"));
        assert_eq!(src("routing.combos[2].name").as_deref(), Some("local"));
        assert_eq!(src("token_saver.ponytail").as_deref(), Some("project"));
        assert_eq!(src("token_saver.caveman").as_deref(), Some("user"));
        assert_eq!(src("token_saver.rtk_enabled").as_deref(), Some("user"));
    }

    #[test]
    fn routing_resolve_subagent_model_precedence() {
        let mut r = RoutingSettings {
            main_model: Some("main".into()),
            subagent_model: Some("sub-default".into()),
            explore_model: Some("explore-default".into()),
            plan_model: Some("plan-default".into()),
            role_models: BTreeMap::from([
                ("explore".into(), "role-explore".into()),
                ("custom".into(), "role-custom".into()),
                ("empty-role".into(), "   ".into()),
            ]),
            combos: Vec::new(),
        };

        assert_eq!(
            r.resolve_subagent_model("explore", Some("exp"), Some("def"), "parent"),
            "exp"
        );
        assert_eq!(
            r.resolve_subagent_model("explore", Some("  "), Some("def"), "parent"),
            "def"
        );
        assert_eq!(
            r.resolve_subagent_model("explore", Some(""), Some(""), "parent"),
            "role-explore"
        );
        assert_eq!(
            r.resolve_subagent_model("custom", None, None, "parent"),
            "role-custom"
        );

        r.role_models.remove("explore");
        assert_eq!(
            r.resolve_subagent_model("explore", None, None, "parent"),
            "explore-default"
        );
        assert_eq!(
            r.resolve_subagent_model("plan", None, None, "parent"),
            "plan-default"
        );
        assert_eq!(
            r.resolve_subagent_model("other", None, None, "parent"),
            "sub-default"
        );
        assert_eq!(
            r.resolve_subagent_model("empty-role", None, None, "parent"),
            "sub-default"
        );

        r.explore_model = Some("".into());
        assert_eq!(
            r.resolve_subagent_model("explore", None, None, "parent"),
            "sub-default"
        );

        r.subagent_model = Some(" ".into());
        assert_eq!(
            r.resolve_subagent_model("other", None, None, "parent"),
            "parent"
        );

        r.subagent_model = None;
        assert_eq!(
            r.resolve_subagent_model("other", None, None, "parent"),
            "parent"
        );
    }

    #[test]
    fn env_vars_override_subagent_model_and_token_saver() {
        let dir = scratch();
        let paths = Paths::with_root(dir.path().join("data"));
        write(
            &paths.settings(),
            "[routing]\nsubagent_model = \"from-file\"\n[token_saver]\nponytail = \"lite\"\ncaveman = \"lite\"\n",
        );
        let env = |k: &str| match k {
            "ZERITH_SUBAGENT_MODEL" => Some("from-env".to_string()),
            "ZERITH_PONYTAIL" => Some("ultra".to_string()),
            "ZERITH_CAVEMAN" => Some("wenyan".to_string()),
            _ => None,
        };
        let l = Settings::load_with_env(&paths, None, env);
        assert!(l.errors.is_empty(), "{:?}", l.errors);
        assert_eq!(
            l.settings.routing.subagent_model.as_deref(),
            Some("from-env")
        );
        assert_eq!(l.settings.token_saver.ponytail, PonytailLevel::Ultra);
        assert_eq!(l.settings.token_saver.caveman, CavemanLevel::Wenyan);
        assert!(l.settings.token_saver.rtk_enabled);

        let entries = l.entries();
        let src = |k: &str| entries.iter().find(|e| e.0 == k).map(|e| e.2.clone());
        assert_eq!(src("routing.subagent_model").as_deref(), Some("env"));
        assert_eq!(src("token_saver.ponytail").as_deref(), Some("env"));
        assert_eq!(src("token_saver.caveman").as_deref(), Some("env"));
        assert_eq!(src("token_saver.rtk_enabled").as_deref(), Some("default"));
    }

    #[test]
    fn token_saver_levels_round_trip() {
        for level in [
            PonytailLevel::Off,
            PonytailLevel::Lite,
            PonytailLevel::Full,
            PonytailLevel::Ultra,
        ] {
            assert_eq!(PonytailLevel::parse(level.as_str()), Some(level));
        }
        assert_eq!(PonytailLevel::parse(" FULL "), Some(PonytailLevel::Full));
        assert_eq!(PonytailLevel::parse("invalid"), None);

        for level in [
            CavemanLevel::Off,
            CavemanLevel::Lite,
            CavemanLevel::Full,
            CavemanLevel::Ultra,
            CavemanLevel::Wenyan,
        ] {
            assert_eq!(CavemanLevel::parse(level.as_str()), Some(level));
        }
        assert_eq!(CavemanLevel::parse(" WenYan "), Some(CavemanLevel::Wenyan));
        assert_eq!(CavemanLevel::parse("invalid"), None);
    }
}
