//! mTLS towards regency's endpoint: the client certificate in each `TlsMode`, and
//! the gRPC channel.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

use regent_sdk::proto::grpc::reporting::reporting_client::ReportingClient;
use regent_sdk::proto::{EnrollRequest, RenewCertificateRequest};
use sha2::{Digest, Sha256};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tracing::{info, warn};

use crate::common::TlsMode;
use crate::config::RegencyConfig;

/// The client identity to present, obtaining or renewing it first when the mode
/// requires it.
pub async fn client_identity(config: &RegencyConfig, host_id: &str) -> Result<Identity, String> {
    match config.tls_mode {
        TlsMode::ExternalCa => read_identity(config),
        TlsMode::SelfSigned => {
            if !Path::new(&config.cert_path).exists() || !Path::new(&config.key_path).exists() {
                let (cert_pem, key_pem) = self_signed(host_id)?;
                write_identity(config, &cert_pem, &key_pem)?;
                info!(
                    fingerprint = %fingerprint(&cert_pem)?,
                    "Generated a self-signed client certificate: approve this fingerprint in regency"
                );
            }
            read_identity(config)
        }
        TlsMode::BuiltInCa => built_in_ca(config, host_id).await,
    }
}

/// gRPC channel to the endpoint, with the given client identity (none: only for
/// enrollment).
pub async fn channel(config: &RegencyConfig, identity: Option<Identity>) -> Result<Channel, String> {
    let ca = std::fs::read(&config.ca_cert_path)
        .map_err(|e| format!("Failed to read {}: {}", config.ca_cert_path, e))?;
    let mut tls = ClientTlsConfig::new().ca_certificate(Certificate::from_pem(ca));
    if let Some(name) = &config.server_name {
        tls = tls.domain_name(name.clone());
    }
    if let Some(identity) = identity {
        tls = tls.identity(identity);
    }
    let timeout = Duration::from_secs(config.request_timeout_sec);
    Endpoint::from_shared(config.endpoint.clone())
        .map_err(|e| format!("Invalid regency.endpoint: {}", e))?
        .tls_config(tls)
        .map_err(|e| format!("Invalid TLS configuration: {}", e))?
        .connect_timeout(timeout)
        .timeout(timeout)
        .connect()
        .await
        .map_err(|e| format!("Failed to connect to {}: {}", config.endpoint, e))
}

/// SHA-256 fingerprint of the first certificate of a PEM bundle
/// (`AB:CD:…`), as shown by regency for approval.
pub fn fingerprint(cert_pem: &str) -> Result<String, String> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
        .map_err(|e| format!("Invalid certificate PEM: {}", e))?;
    let digest = Sha256::digest(&pem.contents);
    Ok(digest
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":"))
}

async fn built_in_ca(config: &RegencyConfig, host_id: &str) -> Result<Identity, String> {
    let now = chrono::Utc::now().timestamp();
    let renew_before = i64::try_from(config.renew_before_sec).unwrap_or(i64::MAX);
    match current_not_after(config) {
        Some(not_after) if not_after.saturating_sub(now) > renew_before => read_identity(config),
        Some(not_after) if not_after > now => {
            if let Err(details) = renew(config, host_id).await {
                warn!(details, "Certificate renewal failed; keeping the current one until it expires");
            }
            read_identity(config)
        }
        _ => {
            enroll(config, host_id).await?;
            read_identity(config)
        }
    }
}

async fn enroll(config: &RegencyConfig, host_id: &str) -> Result<(), String> {
    let token_path = config.enrollment_token_path.as_deref().ok_or(
        "TlsMode BuiltInCa: no valid certificate yet and regency.enrollment_token_path is unset",
    )?;
    let token = std::fs::read_to_string(token_path)
        .map_err(|e| format!("Failed to read enrollment token {}: {}", token_path, e))?;
    let (csr_der, key_pem) = signing_request(host_id)?;

    let mut request = EnrollRequest::new();
    request.set_enrollment_token(token.trim());
    request.set_csr_der(csr_der);
    let mut client = ReportingClient::new(channel(config, None).await?);
    let response = client
        .enroll(request)
        .await
        .map_err(|status| format!("Enrollment refused: {}", status))?
        .into_inner();
    let cert_pem = response.certificate_chain_pem().to_string();
    write_identity(config, &cert_pem, &key_pem)?;
    info!(fingerprint = %fingerprint(&cert_pem)?, "Enrolled with regency's built-in CA");
    Ok(())
}

async fn renew(config: &RegencyConfig, host_id: &str) -> Result<(), String> {
    let (csr_der, key_pem) = signing_request(host_id)?;
    let mut request = RenewCertificateRequest::new();
    request.set_csr_der(csr_der);
    let mut client = ReportingClient::new(channel(config, Some(read_identity(config)?)).await?);
    let response = client
        .renew_certificate(request)
        .await
        .map_err(|status| format!("Renewal refused: {}", status))?
        .into_inner();
    let cert_pem = response.certificate_chain_pem().to_string();
    write_identity(config, &cert_pem, &key_pem)?;
    info!("Client certificate renewed");
    Ok(())
}

/// `not_after` (Unix time) of the current client certificate, if readable.
fn current_not_after(config: &RegencyConfig) -> Option<i64> {
    if !Path::new(&config.key_path).exists() {
        return None;
    }
    let raw = std::fs::read(&config.cert_path).ok()?;
    let (_, pem) = x509_parser::pem::parse_x509_pem(&raw).ok()?;
    let cert = pem.parse_x509().ok()?;
    Some(cert.validity().not_after.timestamp())
}

fn read_identity(config: &RegencyConfig) -> Result<Identity, String> {
    let cert = std::fs::read(&config.cert_path)
        .map_err(|e| format!("Failed to read client certificate {}: {}", config.cert_path, e))?;
    let key = std::fs::read(&config.key_path)
        .map_err(|e| format!("Failed to read client key {}: {}", config.key_path, e))?;
    Ok(Identity::from_pem(cert, key))
}

/// Fresh key pair and CSR (DER) whose subject and DNS SAN are the host id.
fn signing_request(host_id: &str) -> Result<(Vec<u8>, String), String> {
    let key = rcgen::KeyPair::generate().map_err(|e| format!("Failed to generate a key: {}", e))?;
    let params = certificate_params(host_id)?;
    let csr = params
        .serialize_request(&key)
        .map_err(|e| format!("Failed to build the certificate request: {}", e))?;
    Ok((csr.der().to_vec(), key.serialize_pem()))
}

fn self_signed(host_id: &str) -> Result<(String, String), String> {
    let key = rcgen::KeyPair::generate().map_err(|e| format!("Failed to generate a key: {}", e))?;
    let cert = certificate_params(host_id)?
        .self_signed(&key)
        .map_err(|e| format!("Failed to self-sign a certificate: {}", e))?;
    Ok((cert.pem(), key.serialize_pem()))
}

fn certificate_params(host_id: &str) -> Result<rcgen::CertificateParams, String> {
    let mut params = rcgen::CertificateParams::new(vec![host_id.to_string()]).map_err(|e| {
        format!("Host id '{host_id}' cannot be a certificate name (it must be a valid DNS name): {e}")
    })?;
    let mut name = rcgen::DistinguishedName::new();
    name.push(rcgen::DnType::CommonName, host_id);
    params.distinguished_name = name;
    Ok(params)
}

/// Write key (0600) and certificate atomically next to their final paths.
fn write_identity(config: &RegencyConfig, cert_pem: &str, key_pem: &str) -> Result<(), String> {
    for path in [&config.cert_path, &config.key_path] {
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
        }
    }
    let key_tmp = format!("{}.tmp", config.key_path);
    let cert_tmp = format!("{}.tmp", config.cert_path);
    let mut key_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&key_tmp)
        .map_err(|e| format!("Failed to write {}: {}", key_tmp, e))?;
    key_file
        .write_all(key_pem.as_bytes())
        .map_err(|e| format!("Failed to write {}: {}", key_tmp, e))?;
    std::fs::write(&cert_tmp, cert_pem).map_err(|e| format!("Failed to write {}: {}", cert_tmp, e))?;
    std::fs::rename(&key_tmp, &config.key_path).map_err(|e| format!("Failed to install the key: {}", e))?;
    std::fs::rename(&cert_tmp, &config.cert_path)
        .map_err(|e| format!("Failed to install the certificate: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_signed_certificate_names_the_host() {
        let (cert_pem, key_pem) = self_signed("web-01").unwrap();
        assert!(key_pem.contains("PRIVATE KEY"));
        let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes()).unwrap();
        let cert = pem.parse_x509().unwrap();
        assert!(cert.subject().to_string().contains("web-01"));
        assert_eq!(fingerprint(&cert_pem).unwrap().split(':').count(), 32);
    }

    #[test]
    fn signing_request_is_der() {
        let (csr_der, _) = signing_request("web-01").unwrap();
        assert_eq!(csr_der[0], 0x30, "DER SEQUENCE");
    }
}
