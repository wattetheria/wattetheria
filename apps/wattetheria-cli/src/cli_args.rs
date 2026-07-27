use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "wattetheria", bin_name = "wattetheria")]
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
    /// Manage autonomous network registration.
    Network {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: NetworkRegistrationCommand,
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
    /// Initialize or inspect the Wattetheria node's local Agent identity.
    Identity {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: IdentityCommand,
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
pub(crate) enum NetworkRegistrationCommand {
    AuthorityInit,
    AuthorityShow,
    CreateRequest {
        #[arg(long)]
        network_did: String,
        #[arg(long)]
        network_id: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        mainnet_did: String,
        #[arg(long = "federation-endpoint", value_name = "TRANSPORT=ENDPOINT")]
        federation_endpoints: Vec<String>,
        #[arg(long, default_value_t = 30)]
        valid_for_days: u64,
        #[arg(long)]
        out: PathBuf,
    },
    InspectRequest {
        #[arg(long)]
        request: PathBuf,
    },
    ExportTrustBundle {
        #[arg(long)]
        mainnet_did: String,
        #[arg(long)]
        network_id: String,
        #[arg(long)]
        out: PathBuf,
    },
    IssueCredential {
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        mainnet_did: String,
        #[arg(long)]
        network_id: String,
        #[arg(long, default_value_t = 365)]
        valid_for_days: u64,
        #[arg(long)]
        out: PathBuf,
    },
    VerifyCredential {
        #[arg(long)]
        credential: PathBuf,
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        trust_bundle: PathBuf,
    },
    ImportCredential {
        #[arg(long)]
        credential: PathBuf,
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        trust_bundle: PathBuf,
    },
    ListCredentials {
        #[arg(long)]
        subject_network_did: Option<String>,
    },
    RevokeCredential {
        #[arg(long)]
        credential_id: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        mainnet_did: String,
        #[arg(long)]
        out: PathBuf,
    },
    VerifyRevocation {
        #[arg(long)]
        revocation: PathBuf,
        #[arg(long)]
        trust_bundle: PathBuf,
    },
    ImportRevocation {
        #[arg(long)]
        revocation: PathBuf,
        #[arg(long)]
        trust_bundle: PathBuf,
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
pub(crate) enum IdentityCommand {
    /// Initialize the node's local Agent identity.
    Init,
    /// Show the local identity public DID and public key.
    Show,
    /// Export the local identity seed; treat it like a password.
    ExportSeed,
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

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    #[test]
    fn cli_exposes_network_commands_under_the_public_binary_name() {
        let command = Cli::command();
        let network = command
            .get_subcommands()
            .find(|subcommand| subcommand.get_name() == "network");

        assert_eq!(command.get_bin_name(), Some("wattetheria"));
        assert!(network.is_some());
        assert!(
            command
                .get_subcommands()
                .all(|subcommand| subcommand.get_name() != "network-registration")
        );
    }
}
