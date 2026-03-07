use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "wattetheria")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Commands {
    Init {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
    },
    Up {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[arg(long)]
        control_plane_bind: Option<String>,
        #[arg(long, default_value_t = false)]
        attach: bool,
    },
    Doctor {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[arg(long)]
        control_plane: Option<String>,
        #[arg(long, default_value_t = false)]
        brain: bool,
    },
    Policy {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[arg(long)]
        control_plane: Option<String>,
        #[command(subcommand)]
        command: PolicyCommand,
    },
    Governance {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[arg(long)]
        control_plane: Option<String>,
        #[command(subcommand)]
        command: GovernanceCommand,
    },
    Skill {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: SkillCommand,
    },
    Mcp {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: McpCommand,
    },
    Brain {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: BrainCommand,
    },
    Data {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: DataCommand,
    },
    Oracle {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: OracleCommand,
    },
    UpgradeCheck {
        #[arg(long, default_value = env!("CARGO_PKG_VERSION"))]
        current: String,
        #[arg(long)]
        latest: Option<String>,
    },
    NightShift {
        #[arg(long, default_value = ".wattetheria/events.jsonl")]
        event_log: PathBuf,
        #[arg(long, default_value_t = 12)]
        hours: i64,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    PostSummary {
        #[arg(long, default_value = ".wattetheria/identity.json")]
        identity: PathBuf,
        #[arg(long, default_value = ".wattetheria/events.jsonl")]
        events: PathBuf,
        #[arg(long)]
        subnet: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:8787/api/summaries")]
        endpoint: String,
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum PolicyCommand {
    Check {
        #[arg(long)]
        subject: String,
        #[arg(long, value_enum)]
        trust: TrustArg,
        #[arg(long)]
        capability: String,
        #[arg(long)]
        reason: Option<String>,
    },
    Pending,
    Approve {
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        approved_by: String,
        #[arg(long, value_enum)]
        scope: ScopeArg,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum GovernanceCommand {
    Planets,
    Proposals {
        #[arg(long)]
        subnet_id: Option<String>,
    },
    Propose {
        #[arg(long)]
        subnet_id: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        created_by: String,
        #[arg(long, default_value = "{}")]
        payload: String,
    },
    Vote {
        #[arg(long)]
        proposal_id: String,
        #[arg(long)]
        voter: String,
        #[arg(long)]
        approve: bool,
    },
    Finalize {
        #[arg(long)]
        proposal_id: String,
        #[arg(long, default_value_t = 1)]
        min_votes_for: usize,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum SkillCommand {
    Install {
        source: String,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    Perms {
        id: String,
    },
    Test {
        id: String,
        #[arg(long, default_value = "{\"hello\":\"world\"}")]
        input: String,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommand {
    Add {
        config: PathBuf,
    },
    Enable {
        server: String,
    },
    Disable {
        server: String,
    },
    List,
    Test {
        server: String,
        tool: String,
        #[arg(long, default_value = "{}")]
        input: String,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum BrainCommand {
    HumanizeNightShift {
        #[arg(long, default_value_t = 12)]
        hours: i64,
    },
    ProposeActions,
    PlanSkillCalls {
        #[arg(long, default_value_t = false)]
        enable: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum DataCommand {
    SnapshotCreate,
    Recover {
        #[arg(long = "source")]
        source: Vec<PathBuf>,
    },
    Migrate {
        #[arg(long, default_value = "0.2.0")]
        to: String,
    },
    BackupExport {
        out: PathBuf,
    },
    BackupImport {
        input: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum OracleCommand {
    Publish {
        feed_id: String,
        #[arg(long, default_value = "{}")]
        payload: String,
        #[arg(long, default_value_t = 1)]
        price_watt: i64,
    },
    Subscribe {
        feed_id: String,
        #[arg(long, default_value_t = 1)]
        max_price_watt: i64,
    },
    Credit {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        watt: i64,
    },
    Balance {
        #[arg(long)]
        agent: Option<String>,
    },
    Pull {
        feed_id: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum TrustArg {
    Trusted,
    Verified,
    Untrusted,
}

impl TrustArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Verified => "verified",
            Self::Untrusted => "untrusted",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ScopeArg {
    Once,
    Session,
    Permanent,
}

impl ScopeArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Session => "session",
            Self::Permanent => "permanent",
        }
    }
}
