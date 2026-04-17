// SPDX-License-Identifier: MIT OR Apache-2.0

//! Credential subcommands: issue, verify, inspect.
//!
//! These exercise the CDN peer credential system — issuing signed
//! delegations from a group master to mirrors/peers, and verifying
//! them.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Subcommand;

use p2p_distribute::catalog_sign::{HmacSha256Signer, HmacSha256Verifier};
use p2p_distribute::credential::PeerCredential;
use p2p_distribute::group::GroupRole;
use p2p_distribute::network_id::NetworkId;
use p2p_distribute::peer_id::PeerId;

/// Peer credential operations.
#[derive(Subcommand)]
pub enum CredentialCommand {
    /// Issue a new peer credential (signs with HMAC-SHA256 key file).
    Issue {
        /// Network/group name.
        #[arg(short, long)]
        network: String,

        /// Role to grant: master, admin, mirror, reader.
        #[arg(short, long, default_value = "mirror")]
        role: String,

        /// Path to the HMAC key file (raw bytes).
        #[arg(short, long)]
        key: PathBuf,

        /// Validity duration in hours (default: 168 = 1 week).
        #[arg(long, default_value = "168")]
        hours: u64,

        /// Output file for the credential (canonical bytes + signature).
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Verify a credential file against a key.
    Verify {
        /// Path to the credential file.
        file: PathBuf,

        /// Path to the HMAC key file (raw bytes).
        #[arg(short, long)]
        key: PathBuf,
    },

    /// Display credential metadata without verification.
    Inspect {
        /// Path to the credential file.
        file: PathBuf,
    },
}

/// Run the credential subcommand.
pub fn run(cmd: CredentialCommand) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        CredentialCommand::Issue {
            network,
            role,
            key,
            hours,
            output,
        } => cmd_issue(&network, &role, &key, hours, &output),
        CredentialCommand::Verify { file, key } => cmd_verify(&file, &key),
        CredentialCommand::Inspect { file } => cmd_inspect(&file),
    }
}

// ── Wire format ─────────────────────────────────────────────────────
//
// Credential file layout (simple, no framing overhead):
//
//   [0..82)    canonical bytes (fixed, self-describing via version)
//   [82..84)   signature length as u16 LE
//   [84..84+S) signature bytes
//   [84+S..)   issuer_id bytes (remainder)
//
// This format is intentionally simple — no TLV, no protobuf. It's a
// debugging/testing tool, not a production wire format.

/// Serialize a credential to the file format.
fn serialize_credential(cred: &PeerCredential) -> Vec<u8> {
    let canonical = cred.canonical_bytes();
    let sig = cred.signature();
    let issuer = cred.issuer_id();
    let sig_len = sig.len().min(u16::MAX as usize) as u16;

    let mut out = Vec::with_capacity(canonical.len() + 2 + sig.len() + issuer.len());
    out.extend_from_slice(&canonical);
    out.extend_from_slice(&sig_len.to_le_bytes());
    out.extend_from_slice(sig);
    out.extend_from_slice(issuer);
    out
}

/// Deserialize a credential from the file format.
fn deserialize_credential(data: &[u8]) -> Result<PeerCredential, Box<dyn std::error::Error>> {
    // Canonical bytes are 82 bytes, then 2-byte sig length, then sig, then issuer.
    if data.len() < 84 {
        return Err("credential file too short (need at least 84 bytes)".into());
    }

    let canonical = data.get(..82).ok_or("truncated canonical bytes")?;

    let sig_len_bytes: [u8; 2] = data
        .get(82..84)
        .and_then(|s| s.try_into().ok())
        .ok_or("truncated signature length")?;
    let sig_len = u16::from_le_bytes(sig_len_bytes) as usize;

    let sig_end = 84usize
        .checked_add(sig_len)
        .ok_or("signature length overflow")?;
    if data.len() < sig_end {
        return Err(format!(
            "credential file truncated: need {} bytes for signature, have {}",
            sig_end,
            data.len()
        )
        .into());
    }

    let signature = data.get(84..sig_end).unwrap_or_default().to_vec();
    let issuer_id = data.get(sig_end..).unwrap_or_default().to_vec();

    let cred = PeerCredential::from_canonical(canonical, signature, issuer_id)?;
    Ok(cred)
}

/// Parse a role string into a GroupRole.
fn parse_role(s: &str) -> Result<GroupRole, Box<dyn std::error::Error>> {
    match s.to_lowercase().as_str() {
        "master" => Ok(GroupRole::Master),
        "admin" => Ok(GroupRole::Admin),
        "mirror" => Ok(GroupRole::Mirror),
        "reader" => Ok(GroupRole::Reader),
        other => {
            Err(format!("unknown role '{other}': expected master, admin, mirror, or reader").into())
        }
    }
}

/// Get current Unix timestamp in seconds.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Format a GroupRole for display.
fn role_display(role: GroupRole) -> &'static str {
    match role {
        GroupRole::Master => "master",
        GroupRole::Admin => "admin",
        GroupRole::Mirror => "mirror",
        GroupRole::Reader => "reader",
    }
}

// ── issue ───────────────────────────────────────────────────────────

/// Issue a signed credential for a randomly generated peer identity.
fn cmd_issue(
    network: &str,
    role_str: &str,
    key_path: &std::path::Path,
    hours: u64,
    output: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let role = parse_role(role_str)?;
    let key_data = fs::read(key_path)?;
    let signer = HmacSha256Signer::new(&key_data);

    let peer = PeerId::generate()?;
    let network_id = NetworkId::from_name(network);
    let now = now_secs();
    let expires = now.saturating_add(hours.saturating_mul(3600));

    let mut cred = PeerCredential::new(peer, network_id, role, now, expires);
    cred.sign(&signer);

    let serialized = serialize_credential(&cred);
    fs::write(output, &serialized)?;

    println!("Credential issued:");
    println!("  Subject:  {peer}");
    println!("  Network:  {network} ({network_id})");
    println!("  Role:     {}", role_display(role));
    println!("  Issued:   {now} (Unix seconds)");
    println!("  Expires:  {expires} (Unix seconds, {hours}h from now)");
    println!(
        "  Written:  {} ({} bytes)",
        output.display(),
        serialized.len()
    );

    Ok(())
}

// ── verify ──────────────────────────────────────────────────────────

/// Verify a credential file against a key.
fn cmd_verify(
    file: &std::path::Path,
    key_path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let data = fs::read(file)?;
    let cred = deserialize_credential(&data)?;

    let key_data = fs::read(key_path)?;
    let verifier = HmacSha256Verifier::new(&key_data);

    let now = now_secs();
    match cred.verify(&verifier, now) {
        Ok(()) => {
            println!("VALID");
            println!("  Subject: {}", cred.subject());
            println!("  Role:    {}", role_display(cred.role()));
            println!("  Expires: {} (Unix seconds)", cred.expires_at());
            Ok(())
        }
        Err(e) => {
            println!("INVALID: {e}");
            Err(e.into())
        }
    }
}

// ── inspect ─────────────────────────────────────────────────────────

/// Display credential metadata without signature verification.
fn cmd_inspect(file: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let data = fs::read(file)?;
    let cred = deserialize_credential(&data)?;

    let now = now_secs();

    println!("Subject:     {}", cred.subject());
    println!("Network:     {}", cred.network_id());
    println!("Role:        {}", role_display(cred.role()));
    println!("Issued at:   {} (Unix seconds)", cred.issued_at());
    println!("Expires at:  {} (Unix seconds)", cred.expires_at());
    println!(
        "Signed:      {}",
        if cred.is_signed() { "yes" } else { "no" }
    );
    println!(
        "Expired:     {}",
        if cred.is_expired(now) { "YES" } else { "no" }
    );
    println!("Sig bytes:   {}", cred.signature().len());
    println!("Issuer bytes:{}", cred.issuer_id().len());

    Ok(())
}
