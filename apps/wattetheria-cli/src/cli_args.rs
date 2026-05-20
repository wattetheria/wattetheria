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
    Wallet {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: WalletCommand,
    },
    Identity {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: IdentityCommand,
    },
    Servicenet {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: ServicenetCommand,
    },
    Publish {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        /// Path to A2A `AgentCard` JSON file.
        card: PathBuf,
        /// Public endpoint URL where the agent runs.
        #[arg(long)]
        endpoint: String,
        /// Provider id this agent belongs to (must already be registered).
        #[arg(long)]
        provider_id: String,
        /// Agent id within the provider namespace.
        #[arg(long)]
        agent_id: String,
        /// Semantic version of this submission, e.g. "0.1.0".
        #[arg(long, default_value = "0.1.0")]
        version: String,
        /// Servicenet base URL, e.g. <https://servicenet.wattetheria.network>
        #[arg(long)]
        servicenet: String,
        /// Risk level: low | medium | high.
        #[arg(long, default_value = "low")]
        risk_level: String,
        /// Skip building a `PaymentAccountBindingProof` from the active wallet
        /// payment account. Use this for agents that do not collect payments;
        /// callers will not have a verified payment binding for them.
        #[arg(long, default_value_t = false)]
        skip_binding_proof: bool,
        /// How many minutes the signed submission stays valid. Defaults to 30.
        #[arg(long, default_value_t = 30)]
        ttl_minutes: u64,
        /// Print signed request without sending.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
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
    CreatePaymentAccount {
        #[arg(long)]
        label: Option<String>,
        #[arg(long, default_value = "x402")]
        rail: String,
        #[arg(long)]
        network: Option<String>,
    },
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
    ListPaymentAccounts,
    BindPaymentAccount {
        #[arg(long)]
        account_id: String,
    },
    ActivePaymentAccount,
}

#[derive(Debug, Subcommand)]
pub(crate) enum IdentityCommand {
    /// Generate and persist a wallet-backed agent identity if missing,
    /// then print the DID.
    Init,
    /// Print the public agent DID + public key, never the private key.
    Show,
    /// Export the active ed25519 identity seed as 32 raw bytes, hex-encoded.
    /// Treat the output like a password.
    ExportSeed,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ServicenetCommand {
    Provider {
        #[command(subcommand)]
        command: ServicenetProviderCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ServicenetProviderCommand {
    /// Register the local identity as a provider on a watt-servicenet node.
    Register {
        /// Provider id (namespace), e.g. "alice" or "acme-labs".
        #[arg(long)]
        provider_id: String,
        /// Optional display name shown in registry listings.
        #[arg(long)]
        display_name: Option<String>,
        /// Servicenet base URL, e.g. <https://servicenet.wattetheria.network>
        #[arg(long)]
        servicenet: String,
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
