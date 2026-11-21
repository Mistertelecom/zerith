use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    TriggerCron,
    TriggerManual,
    Shell,
    SshCommand,
    HttpRequest,
    AiAgent,
    Condition,
    Transform,
    NotifyChannel,
    SaveFile,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TriggerCron => "trigger_cron",
            Self::TriggerManual => "trigger_manual",
            Self::Shell => "shell",
            Self::SshCommand => "ssh_command",
            Self::HttpRequest => "http_request",
            Self::AiAgent => "ai_agent",
            Self::Condition => "condition",
            Self::Transform => "transform",
            Self::NotifyChannel => "notify_channel",
            Self::SaveFile => "save_file",
        }
    }

    pub fn is_trigger(&self) -> bool {
        matches!(self, Self::TriggerCron | Self::TriggerManual)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNode {
    pub id: String,
    pub kind: NodeKind,
    pub label: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub config: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEdge {
    pub id: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_icon")]
    pub icon: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub nodes: Vec<WorkflowNode>,
    #[serde(default)]
    pub edges: Vec<WorkflowEdge>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default)]
    pub last_run_at_ms: Option<i64>,
    #[serde(default)]
    pub last_status: Option<String>,
}

fn default_icon() -> String {
    "workflow".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    EvolutionApi,
    Waha,
    #[serde(rename = "whatsapp_cloud")]
    WhatsAppCloud,
    Telegram,
    DiscordWebhook,
    SlackWebhook,
    GenericWebhook,
    DesktopNotify,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    pub id: String,
    pub name: String,
    pub kind: ChannelKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub config: Value,
    pub updated_at_ms: i64,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRunLog {
    pub node_id: String,
    pub label: String,
    pub kind: NodeKind,
    pub status: String,
    pub output: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub id: String,
    pub workflow_id: String,
    pub trigger_source: String,
    pub status: String,
    pub started_at_ms: i64,
    #[serde(default)]
    pub finished_at_ms: Option<i64>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub node_logs: Vec<NodeRunLog>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeEvent {
    pub workflow_id: String,
    pub run_id: String,
    pub node_id: String,
    pub status: String,
    pub output: String,
}
