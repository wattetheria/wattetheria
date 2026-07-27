use crate::cli_args::NetworkRegistrationCommand;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;
use wattetheria_kernel::network_registration::{
    FederationEndpoint, MainnetTrustBundle, MainnetTrustBundlePayload,
    NETWORK_REGISTRATION_PROTOCOL_VERSION, NetworkManifestPayload, NetworkMembershipCredential,
    NetworkMembershipCredentialPayload, NetworkMembershipRevocation,
    NetworkMembershipRevocationPayload, NetworkRegistrationRequest,
    NetworkRegistrationRequestPayload, NetworkRegistrationStore, load_network_authority,
    load_or_create_network_authority, manifest_hash, sign_membership_credential,
    sign_membership_revocation, sign_network_manifest, sign_registration_request,
    sign_trust_bundle, verify_membership_credential, verify_membership_revocation,
    verify_registration_request, verify_trust_bundle,
};

pub(crate) fn run_network_registration(
    data_dir: &Path,
    command: NetworkRegistrationCommand,
) -> Result<()> {
    let now_ms = now_ms();
    let store = NetworkRegistrationStore::new(data_dir);
    match command {
        NetworkRegistrationCommand::AuthorityInit => {
            let authority = load_or_create_network_authority(data_dir)?;
            print_json(&serde_json::json!({
                "authority_did": authority.agent_did,
                "public_key": authority.public_key,
            }))
        }
        NetworkRegistrationCommand::AuthorityShow => {
            let authority = load_network_authority(data_dir)?;
            print_json(&serde_json::json!({
                "authority_did": authority.agent_did,
                "public_key": authority.public_key,
            }))
        }
        command @ NetworkRegistrationCommand::CreateRequest { .. } => {
            create_request(data_dir, &store, now_ms, command)
        }
        NetworkRegistrationCommand::InspectRequest { request } => {
            inspect_request(data_dir, now_ms, &request)
        }
        NetworkRegistrationCommand::ExportTrustBundle {
            mainnet_did,
            network_id,
            out,
        } => export_trust_bundle(data_dir, now_ms, mainnet_did, network_id, &out),
        command @ NetworkRegistrationCommand::IssueCredential { .. } => {
            issue_credential(data_dir, &store, now_ms, command)
        }
        NetworkRegistrationCommand::VerifyCredential {
            credential,
            request,
            trust_bundle,
        } => {
            let (credential, _, _) =
                load_credential_evidence(data_dir, &credential, &request, &trust_bundle, now_ms)?;
            print_json(&serde_json::json!({
                "valid": true,
                "credential_id": credential.payload.credential_id,
            }))
        }
        NetworkRegistrationCommand::ImportCredential {
            credential,
            request,
            trust_bundle,
        } => {
            let (credential, request, trust) =
                load_credential_evidence(data_dir, &credential, &request, &trust_bundle, now_ms)?;
            store.store_credential(&credential, &request, &trust)?;
            print_json(&serde_json::json!({
                "credential_id": credential.payload.credential_id,
                "status": "imported",
            }))
        }
        NetworkRegistrationCommand::ListCredentials {
            subject_network_did,
        } => print_json(&store.list_credentials(subject_network_did.as_deref())?),
        command @ NetworkRegistrationCommand::RevokeCredential { .. } => {
            revoke_credential(data_dir, &store, now_ms, command)
        }
        NetworkRegistrationCommand::VerifyRevocation {
            revocation,
            trust_bundle,
        } => {
            let (revocation, _) = load_revocation_evidence(data_dir, &revocation, &trust_bundle)?;
            print_json(&serde_json::json!({
                "valid": true,
                "revocation_id": revocation.payload.revocation_id,
            }))
        }
        NetworkRegistrationCommand::ImportRevocation {
            revocation,
            trust_bundle,
        } => {
            let (revocation, trust) =
                load_revocation_evidence(data_dir, &revocation, &trust_bundle)?;
            let record = store
                .credential(&revocation.payload.credential_id)?
                .context("network membership credential not found")?;
            if record.trust_bundle != trust {
                bail!("revocation trust bundle does not match imported credential evidence");
            }
            store.apply_revocation(&revocation)?;
            print_json(&serde_json::json!({
                "revocation_id": revocation.payload.revocation_id,
                "status": "imported",
            }))
        }
    }
}

fn create_request(
    data_dir: &Path,
    store: &NetworkRegistrationStore,
    now_ms: u64,
    command: NetworkRegistrationCommand,
) -> Result<()> {
    let NetworkRegistrationCommand::CreateRequest {
        network_did,
        network_id,
        name,
        mainnet_did,
        federation_endpoints,
        valid_for_days,
        out,
    } = command
    else {
        unreachable!("create_request requires CreateRequest");
    };
    let authority = load_network_authority(data_dir)?;
    let manifest = sign_network_manifest(
        NetworkManifestPayload {
            protocol_version: NETWORK_REGISTRATION_PROTOCOL_VERSION.to_owned(),
            network_did,
            network_id,
            name,
            authority_did: authority.agent_did.clone(),
            federation_endpoints: federation_endpoints
                .iter()
                .map(|value| parse_endpoint(value))
                .collect::<Result<_>>()?,
            issued_at_ms: now_ms,
            expires_at_ms: None,
        },
        &authority,
    )?;
    let request = sign_registration_request(
        NetworkRegistrationRequestPayload {
            protocol_version: NETWORK_REGISTRATION_PROTOCOL_VERSION.to_owned(),
            request_id: Uuid::new_v4().to_string(),
            target_network_did: mainnet_did,
            manifest,
            issued_at_ms: now_ms,
            expires_at_ms: expiry_after_days(now_ms, valid_for_days)?,
        },
        &authority,
    )?;
    verify_registration_request(&request, now_ms)?;
    store.save_request(&request)?;
    let out = write_json(data_dir, &out, &request)?;
    print_json(&serde_json::json!({
        "request_id": request.payload.request_id,
        "network_did": request.payload.manifest.payload.network_did,
        "signed_by": request.signed_by,
        "out": out,
    }))
}

fn inspect_request(data_dir: &Path, now_ms: u64, path: &Path) -> Result<()> {
    let request: NetworkRegistrationRequest = read_json(data_dir, path)?;
    verify_registration_request(&request, now_ms)?;
    print_json(&serde_json::json!({
        "valid": true,
        "request_id": request.payload.request_id,
        "target_network_did": request.payload.target_network_did,
        "network_did": request.payload.manifest.payload.network_did,
        "network_id": request.payload.manifest.payload.network_id,
        "name": request.payload.manifest.payload.name,
        "authority_did": request.payload.manifest.payload.authority_did,
        "manifest_hash": manifest_hash(&request.payload.manifest)?,
        "expires_at_ms": request.payload.expires_at_ms,
    }))
}

fn export_trust_bundle(
    data_dir: &Path,
    now_ms: u64,
    mainnet_did: String,
    network_id: String,
    out: &Path,
) -> Result<()> {
    let authority = load_network_authority(data_dir)?;
    let bundle = build_trust_bundle(mainnet_did, network_id, now_ms, &authority)?;
    verify_trust_bundle(&bundle)?;
    let out = write_json(data_dir, out, &bundle)?;
    print_json(&serde_json::json!({
        "authority_did": bundle.payload.authority_did,
        "network_did": bundle.payload.network_did,
        "out": out,
    }))
}

fn issue_credential(
    data_dir: &Path,
    store: &NetworkRegistrationStore,
    now_ms: u64,
    command: NetworkRegistrationCommand,
) -> Result<()> {
    let NetworkRegistrationCommand::IssueCredential {
        request,
        mainnet_did,
        network_id,
        valid_for_days,
        out,
    } = command
    else {
        unreachable!("issue_credential requires IssueCredential");
    };
    let request: NetworkRegistrationRequest = read_json(data_dir, &request)?;
    verify_registration_request(&request, now_ms)?;
    if request.payload.target_network_did != mainnet_did {
        bail!("registration request targets a different mainnet DID");
    }
    let authority = load_network_authority(data_dir)?;
    let trust = build_trust_bundle(mainnet_did.clone(), network_id, now_ms, &authority)?;
    if let Some(existing) = store
        .list_credentials(None)?
        .into_iter()
        .find(|record| record.credential.payload.request_id == request.payload.request_id)
    {
        verify_membership_credential(&existing.credential, &request, &trust, now_ms)?;
        let out = write_json(data_dir, &out, &existing.credential)?;
        return print_json(&serde_json::json!({
            "credential_id": existing.credential.payload.credential_id,
            "status": "already_issued",
            "out": out,
        }));
    }
    let credential = sign_membership_credential(
        NetworkMembershipCredentialPayload {
            protocol_version: NETWORK_REGISTRATION_PROTOCOL_VERSION.to_owned(),
            credential_id: Uuid::new_v4().to_string(),
            request_id: request.payload.request_id.clone(),
            issuer_network_did: mainnet_did,
            subject_network_did: request.payload.manifest.payload.network_did.clone(),
            subject_network_id: request.payload.manifest.payload.network_id.clone(),
            subject_authority_did: request.payload.manifest.payload.authority_did.clone(),
            manifest_hash: manifest_hash(&request.payload.manifest)?,
            issued_at_ms: now_ms,
            expires_at_ms: expiry_after_days(now_ms, valid_for_days)?,
        },
        &authority,
    )?;
    verify_membership_credential(&credential, &request, &trust, now_ms)?;
    store.store_credential(&credential, &request, &trust)?;
    let out = write_json(data_dir, &out, &credential)?;
    print_json(&serde_json::json!({
        "credential_id": credential.payload.credential_id,
        "status": "issued",
        "out": out,
    }))
}

fn load_credential_evidence(
    data_dir: &Path,
    credential_path: &Path,
    request_path: &Path,
    trust_bundle_path: &Path,
    now_ms: u64,
) -> Result<(
    NetworkMembershipCredential,
    NetworkRegistrationRequest,
    MainnetTrustBundle,
)> {
    let credential = read_json(data_dir, credential_path)?;
    let request = read_json(data_dir, request_path)?;
    let trust = read_json(data_dir, trust_bundle_path)?;
    verify_membership_credential(&credential, &request, &trust, now_ms)?;
    Ok((credential, request, trust))
}

fn revoke_credential(
    data_dir: &Path,
    store: &NetworkRegistrationStore,
    now_ms: u64,
    command: NetworkRegistrationCommand,
) -> Result<()> {
    let NetworkRegistrationCommand::RevokeCredential {
        credential_id,
        reason,
        mainnet_did,
        out,
    } = command
    else {
        unreachable!("revoke_credential requires RevokeCredential");
    };
    let record = store
        .credential(&credential_id)?
        .context("network membership credential not found")?;
    if record.credential.payload.issuer_network_did != mainnet_did {
        bail!("credential issuer does not match mainnet DID");
    }
    let authority = load_network_authority(data_dir)?;
    if authority.agent_did != record.trust_bundle.payload.authority_did {
        bail!("local network authority does not match credential issuer authority");
    }
    let revocation = sign_membership_revocation(
        NetworkMembershipRevocationPayload {
            protocol_version: NETWORK_REGISTRATION_PROTOCOL_VERSION.to_owned(),
            revocation_id: Uuid::new_v4().to_string(),
            credential_id,
            issuer_network_did: mainnet_did,
            revoked_at_ms: now_ms,
            reason,
        },
        &authority,
    )?;
    verify_membership_revocation(&revocation, &record.trust_bundle)?;
    store.apply_revocation(&revocation)?;
    let out = write_json(data_dir, &out, &revocation)?;
    print_json(&serde_json::json!({
        "revocation_id": revocation.payload.revocation_id,
        "credential_id": revocation.payload.credential_id,
        "status": "revoked",
        "out": out,
    }))
}

fn load_revocation_evidence(
    data_dir: &Path,
    revocation_path: &Path,
    trust_bundle_path: &Path,
) -> Result<(NetworkMembershipRevocation, MainnetTrustBundle)> {
    let revocation = read_json(data_dir, revocation_path)?;
    let trust = read_json(data_dir, trust_bundle_path)?;
    verify_membership_revocation(&revocation, &trust)?;
    Ok((revocation, trust))
}

fn build_trust_bundle(
    mainnet_did: String,
    network_id: String,
    issued_at_ms: u64,
    authority: &wattetheria_kernel::identity::Identity,
) -> Result<MainnetTrustBundle> {
    sign_trust_bundle(
        MainnetTrustBundlePayload {
            protocol_version: NETWORK_REGISTRATION_PROTOCOL_VERSION.to_owned(),
            network_did: mainnet_did,
            network_id,
            authority_did: authority.agent_did.clone(),
            issued_at_ms,
        },
        authority,
    )
}

fn parse_endpoint(value: &str) -> Result<FederationEndpoint> {
    let (transport, endpoint) = value
        .split_once('=')
        .context("federation endpoint must use TRANSPORT=ENDPOINT")?;
    if transport.trim().is_empty() || endpoint.trim().is_empty() {
        bail!("federation endpoint transport and endpoint are required");
    }
    Ok(FederationEndpoint {
        transport: transport.trim().to_owned(),
        endpoint: endpoint.trim().to_owned(),
    })
}

fn expiry_after_days(now_ms: u64, days: u64) -> Result<u64> {
    if days == 0 {
        bail!("valid-for-days must be greater than zero");
    }
    now_ms
        .checked_add(
            days.checked_mul(86_400_000)
                .context("valid-for-days is too large")?,
        )
        .context("registration expiry overflow")
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0).cast_unsigned()
}

fn resolve_data_path(data_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        data_dir.join(path)
    }
}

fn read_json<T: DeserializeOwned>(data_dir: &Path, path: &Path) -> Result<T> {
    let path = resolve_data_path(data_dir, path);
    let raw = fs::read(&path).with_context(|| format!("read JSON from {}", path.display()))?;
    serde_json::from_slice(&raw).with_context(|| format!("parse JSON from {}", path.display()))
}

fn write_json(data_dir: &Path, path: &Path, value: &impl Serialize) -> Result<PathBuf> {
    let path = resolve_data_path(data_dir, path);
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).context("create output directory")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).context("create output temporary file")?;
    temporary
        .write_all(&serde_json::to_vec_pretty(value)?)
        .context("write output temporary file")?;
    temporary.as_file().sync_all().context("sync output file")?;
    temporary
        .persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("install output file {}", path.display()))?;
    Ok(path)
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
