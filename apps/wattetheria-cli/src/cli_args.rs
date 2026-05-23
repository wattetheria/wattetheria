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
        #[arg(long, default_value_t = false)]
        connect: bool,
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
    /// Manage a lightweight local wallet for `ServiceNet` publishing and payment binding.
    ///
    /// Use this when you have not installed a local Wattetheria node. If a local
    /// node is already installed, use the node's existing wallet instead of
    /// creating a separate local wallet.
    Wallet {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: WalletCommand,
    },
    /// Initialize or inspect a lightweight local identity for `ServiceNet` publishing or wallet binding.
    ///
    /// Use this when you have not installed a local Wattetheria node. If a local
    /// node is already installed, use the node's existing identity instead of
    /// creating a separate local identity.
    Identity {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: IdentityCommand,
    },
    /// Register and publish agents to `ServiceNet`.
    Servicenet {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: ServicenetCommand,
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
pub(crate) enum WalletCommand {
    /// Create a local payment account for `ServiceNet` payment binding.
    CreatePaymentAccount {
        #[arg(long)]
        label: Option<String>,
        #[arg(long, default_value = "x402")]
        rail: String,
        #[arg(long)]
        network: Option<String>,
    },
    /// Import a local payment account for `ServiceNet` payment binding.
    ImportPaymentAccount {
        #[arg(long)]
        private_key_hex: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long, default_value = "x402")]
        rail: String,
        #[arg(long)]
        network: Option<String>,
    },
    /// Track a payment address without importing its private key.
    WatchPaymentAccount {
        #[arg(long)]
        address: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long, default_value = "x402")]
        rail: String,
        #[arg(long)]
        network: Option<String>,
    },
    /// List local payment accounts.
    ListPaymentAccounts,
    /// Select the active local payment account for `ServiceNet` payment binding.
    BindPaymentAccount {
        #[arg(long)]
        account_id: String,
    },
    /// Show the active local payment account.
    ActivePaymentAccount,
}

#[derive(Debug, Subcommand)]
pub(crate) enum IdentityCommand {
    /// Initialize a lightweight local identity for `ServiceNet` publishing or wallet binding.
    Init,
    /// Show the local identity public DID and public key.
    Show,
    /// Export the local identity seed; treat it like a password.
    ExportSeed,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ServicenetCommand {
    /// Register an agent card and local identity with `ServiceNet`.
    ///
    /// Returns the `ServiceNet` `agent_id` and `provider_id` used by later publish steps.
    #[command(
        after_help = "Examples:\n  wattetheria servicenet register\n  wattetheria servicenet register --card <path-to-agent-card.jsonc>"
    )]
    Register {
        /// Path to A2A `AgentCard` JSON or JSONC file. Defaults to agent-card.jsonc in the current directory.
        #[arg(long, default_value = "agent-card.jsonc")]
        card: PathBuf,
    },
    /// Generate local agent-card files used by `ServiceNet` registration.
    AgentCard {
        #[command(subcommand)]
        command: ServicenetAgentCardCommand,
    },
    /// Publish a registered `ServiceNet` agent by `agent_id`.
    Publish {
        /// Agent id returned by `servicenet register`.
        agent_id: String,
        /// Semantic version of this submission, e.g. "0.1.0".
        #[arg(long, default_value = "0.1.0")]
        version: String,
        /// Risk level: low | medium | high.
        #[arg(long, default_value = "low")]
        risk_level: String,
        /// How many minutes the signed submission stays valid. Defaults to 30.
        #[arg(long, default_value_t = 30)]
        ttl_minutes: u64,
        /// Print signed request without sending.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ServicenetAgentCardCommand {
    /// Generate an editable A2A `AgentCard` template.
    ///
    /// By default, writes agent-card.jsonc in the current directory.
    Init {
        /// Output directory. Defaults to the current directory.
        #[arg(long)]
        out: Option<PathBuf>,
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
