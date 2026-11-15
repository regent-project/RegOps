//! Reporting outcomes to regency (`Reporting = "Regency"`).
//!
//! Each compliance pass records its `RegentTaskResult` in an on-disk spool
//! ([`Reporter::record`], fast and local: never blocks the compliance loop). A
//! separate task ([`Reporter::run_sender`]) pushes spooled outcomes to the
//! endpoint over mTLS and deletes each one only once the endpoint accepted it;
//! after a failure it retries the same message (same idempotency key) later.
//! While a pass runs, its live view is streamed to the endpoint ([`live`]) and
//! its log is spooled with the outcome.

pub mod live;
mod spool;
pub mod tls;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use regent_sdk::proto::grpc::reporting::reporting_client::ReportingClient;
use regent_sdk::proto::runtime::{Parse, Serialize};
use regent_sdk::proto::{ReportOutcomeRequest, ReporterInfo};
use regent_sdk::hosts::live::{LiveSink, RunLog};
use regent_sdk::task::RegentTaskResult;
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

use crate::config::RegencyConfig;
use spool::Spool;

pub struct Reporter {
    config: RegencyConfig,
    host_id: String,
    spool: Spool,
    wake: Notify,
}

impl Reporter {
    pub fn new(config: &RegencyConfig, host_id: &str) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            spool: Spool::new(&config.spool_dir, config.spool_max_entries)?,
            config: config.clone(),
            host_id: host_id.to_string(),
            wake: Notify::new(),
        }))
    }

    /// The live view of a pass: give the sink to the task, then
    /// [`live::LivePass::finish`] it once the task is over.
    pub fn start_live(&self) -> (LiveSink, live::LivePass) {
        live::LivePass::start(&self.config, &self.host_id)
    }

    /// Spool one outcome (with the pass's log, if kept) for sending.
    pub fn record(
        &self,
        result: &RegentTaskResult,
        source_revision: Option<String>,
        run_log: Option<&RunLog>,
    ) -> Result<(), String> {
        let mut reporter = ReporterInfo::new();
        reporter.set_agent("regops");
        reporter.set_agent_version(env!("CARGO_PKG_VERSION"));
        reporter.set_sdk_version(regent_sdk::VERSION);
        if let Some(revision) = source_revision {
            reporter.set_source_revision(revision);
        }
        let mut request = ReportOutcomeRequest::new();
        request.set_result(result.to_proto());
        request.set_reporter(reporter);
        if let Some(run_log) = run_log.filter(|log| !log.updates().is_empty() || log.is_truncated()) {
            request.set_run_log(run_log.to_proto());
        }
        let bytes = request
            .serialize()
            .map_err(|e| format!("Failed to serialize the outcome: {:?}", e))?;
        self.spool.push(
            result.finished_at().timestamp_micros(),
            result.idempotency_key(),
            &bytes,
        )?;
        self.wake.notify_one();
        Ok(())
    }

    /// Push spooled outcomes forever: right after each record, and every
    /// `retry_interval_sec` while the endpoint cannot be reached.
    pub async fn run_sender(self: Arc<Self>) {
        let retry = Duration::from_secs(self.config.retry_interval_sec);
        loop {
            match self.flush().await {
                Ok(()) => {
                    tokio::select! {
                        _ = self.wake.notified() => {}
                        _ = tokio::time::sleep(retry) => {}
                    }
                }
                Err(details) => {
                    warn!(details, "Failed to report outcomes to regency; will retry");
                    tokio::time::sleep(retry).await;
                }
            }
        }
    }

    /// Send every spooled outcome, oldest first. Stops at the first error that is
    /// not specific to one outcome.
    pub async fn flush(&self) -> Result<(), String> {
        let entries = self.spool.entries()?;
        if entries.is_empty() {
            return Ok(());
        }
        let identity = tls::client_identity(&self.config, &self.host_id).await?;
        let mut client = ReportingClient::new(tls::channel(&self.config, Some(identity)).await?);

        for path in entries {
            let request = match std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| ReportOutcomeRequest::parse(&bytes).map_err(|e| format!("{e:?}")))
            {
                Ok(request) => request,
                Err(details) => {
                    error!(details, path = %path.display(), "Unreadable spooled outcome, set aside");
                    self.spool.reject(&path);
                    continue;
                }
            };
            match client.report_outcome(request).await {
                Ok(_) => {
                    debug!(path = %file_name(&path), "Outcome reported");
                    self.spool.remove(&path);
                }
                Err(status) if status.code() == tonic::Code::InvalidArgument => {
                    error!(
                        reason = status.message(),
                        path = %file_name(&path),
                        "Outcome refused by regency, set aside in the spool's rejected/ directory"
                    );
                    self.spool.reject(&path);
                }
                Err(status) => {
                    return Err(format!("{} ({:?})", status.message(), status.code()));
                }
            }
        }
        info!("Spooled outcomes reported to regency");
        Ok(())
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    //! A local mTLS collector (test CA, client certificates required except for
    //! Enroll) receiving outcomes from a Reporter.

    use super::*;
    use std::sync::Mutex;

    use regent_sdk::proto::grpc::reporting::reporting_server::{Reporting, ReportingServer};
    use regent_sdk::proto::{
        EnrollRequest, EnrollResponse, RenewCertificateRequest, RenewCertificateResponse,
        ReportLiveRequest, ReportLiveResponse, ReportOutcomeResponse,
    };
    use regent_sdk::state::compliance::ManagedHostStatus;
    use regent_sdk::task::Job;
    use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
    use tonic::{Request, Response, Status};

    use crate::common::TlsMode;

    struct Ca(rcgen::CertifiedIssuer<'static, rcgen::KeyPair>);

    impl Ca {
        fn new() -> Self {
            let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            params.distinguished_name.push(rcgen::DnType::CommonName, "regops test CA");
            Self(rcgen::CertifiedIssuer::self_signed(params, rcgen::KeyPair::generate().unwrap()).unwrap())
        }

        fn leaf(&self, name: &str) -> (String, String) {
            let key = rcgen::KeyPair::generate().unwrap();
            let params = rcgen::CertificateParams::new(vec![name.to_string()]).unwrap();
            (params.signed_by(&key, &self.0).unwrap().pem(), key.serialize_pem())
        }

        fn sign_csr(&self, csr_der: &[u8]) -> String {
            let der = rustls::pki_types::CertificateSigningRequestDer::from(csr_der.to_vec());
            let csr = rcgen::CertificateSigningRequestParams::from_der(&der).unwrap();
            csr.signed_by(&self.0).unwrap().pem()
        }
    }

    #[derive(Default)]
    struct Calls {
        reported: Vec<String>,
        /// (host_id, seq) of each live update received
        live: Vec<(String, u64)>,
        /// number of updates in the run log of each outcome
        run_logs: Vec<usize>,
        enrolled: usize,
        renewed: usize,
    }

    struct Collector {
        ca: Arc<Ca>,
        calls: Arc<Mutex<Calls>>,
    }

    #[tonic::async_trait]
    impl Reporting for Collector {
        async fn report_outcome(
            &self,
            request: Request<ReportOutcomeRequest>,
        ) -> Result<Response<ReportOutcomeResponse>, Status> {
            if request.peer_certs().is_none_or(|certs| certs.is_empty()) {
                return Err(Status::unauthenticated("client certificate required"));
            }
            let host_id = request.get_ref().result().host_id().to_string();
            if host_id != "web-01" {
                return Err(Status::invalid_argument("host_id differs from the identity"));
            }
            let mut calls = self.calls.lock().unwrap();
            calls.reported.push(host_id);
            if request.get_ref().has_run_log() {
                calls.run_logs.push(request.get_ref().run_log().updates().len());
            }
            Ok(Response::new(ReportOutcomeResponse::new()))
        }

        async fn report_live(
            &self,
            request: Request<tonic::Streaming<ReportLiveRequest>>,
        ) -> Result<Response<ReportLiveResponse>, Status> {
            if request.peer_certs().is_none_or(|certs| certs.is_empty()) {
                return Err(Status::unauthenticated("client certificate required"));
            }
            let mut stream = request.into_inner();
            let mut accepted = 0;
            while let Some(batch) = stream.message().await? {
                for update in batch.updates() {
                    self.calls
                        .lock()
                        .unwrap()
                        .live
                        .push((update.host_id().to_string(), update.seq()));
                    accepted += 1;
                }
            }
            let mut response = ReportLiveResponse::new();
            response.set_accepted(accepted);
            Ok(Response::new(response))
        }

        async fn enroll(&self, request: Request<EnrollRequest>) -> Result<Response<EnrollResponse>, Status> {
            if request.get_ref().enrollment_token() != "token-web-01" {
                return Err(Status::permission_denied("bad token"));
            }
            self.calls.lock().unwrap().enrolled += 1;
            let mut response = EnrollResponse::new();
            response.set_certificate_chain_pem(self.ca.sign_csr(request.get_ref().csr_der()));
            Ok(Response::new(response))
        }

        async fn renew_certificate(
            &self,
            request: Request<RenewCertificateRequest>,
        ) -> Result<Response<RenewCertificateResponse>, Status> {
            if request.peer_certs().is_none_or(|certs| certs.is_empty()) {
                return Err(Status::unauthenticated("client certificate required"));
            }
            self.calls.lock().unwrap().renewed += 1;
            let mut response = RenewCertificateResponse::new();
            response.set_certificate_chain_pem(self.ca.sign_csr(request.get_ref().csr_der()));
            Ok(Response::new(response))
        }
    }

    struct Setup {
        dir: std::path::PathBuf,
        config: RegencyConfig,
        ca: Arc<Ca>,
        calls: Arc<Mutex<Calls>>,
    }

    async fn setup(name: &str, tls_mode: TlsMode) -> Setup {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let dir = std::env::temp_dir().join(format!("regops-report-test-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let ca = Arc::new(Ca::new());
        std::fs::write(dir.join("ca.pem"), ca.0.pem()).unwrap();
        let (server_cert, server_key) = ca.leaf("localhost");
        let calls = Arc::new(Mutex::new(Calls::default()));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let tls = ServerTlsConfig::new()
            .identity(Identity::from_pem(server_cert, server_key))
            .client_ca_root(Certificate::from_pem(ca.0.pem()))
            .client_auth_optional(true);
        let collector = Collector { ca: ca.clone(), calls: calls.clone() };
        tokio::spawn(
            Server::builder()
                .tls_config(tls)
                .unwrap()
                .add_service(ReportingServer::new(collector))
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
        );

        let path = |file: &str| dir.join(file).to_string_lossy().to_string();
        let config = RegencyConfig {
            endpoint: format!("https://127.0.0.1:{port}"),
            host_id: Some("web-01".to_string()),
            server_name: Some("localhost".to_string()),
            ca_cert_path: path("ca.pem"),
            tls_mode,
            cert_path: path("tls/client.pem"),
            key_path: path("tls/client.key"),
            enrollment_token_path: Some(path("enrollment.token")),
            renew_before_sec: 3600,
            spool_dir: path("spool"),
            spool_max_entries: 10,
            retry_interval_sec: 1,
            request_timeout_sec: 5,
            live: true,
            run_log_max_bytes: 1024 * 1024,
        };
        Setup { dir, config, ca, calls }
    }

    fn result(host_id: &str, key: &str) -> RegentTaskResult {
        let now = chrono::Utc::now();
        RegentTaskResult::from(
            key.to_string(),
            host_id.to_string(),
            Job::Assess,
            now,
            now,
            Ok(ManagedHostStatus::already_compliant()),
        )
    }

    #[tokio::test]
    async fn external_ca_reports_and_sets_refused_outcomes_aside() {
        let setup = setup("external", TlsMode::ExternalCa).await;
        let (cert, key) = setup.ca.leaf("web-01");
        std::fs::create_dir_all(setup.dir.join("tls")).unwrap();
        std::fs::write(&setup.config.cert_path, cert).unwrap();
        std::fs::write(&setup.config.key_path, key).unwrap();

        let reporter = Reporter::new(&setup.config, "web-01").unwrap();
        reporter.record(&result("web-01", "k1"), Some("abc123".to_string()), None).unwrap();
        reporter.record(&result("someone-else", "k2"), None, None).unwrap();
        reporter.flush().await.unwrap();

        assert_eq!(setup.calls.lock().unwrap().reported, vec!["web-01".to_string()]);
        assert!(reporter.spool.entries().unwrap().is_empty());
        assert_eq!(setup.dir.join("spool/rejected").read_dir().unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&setup.dir);
    }

    #[tokio::test]
    async fn built_in_ca_enrolls_then_renews() {
        let mut setup = setup("builtin", TlsMode::BuiltInCa).await;
        std::fs::write(setup.dir.join("enrollment.token"), "token-web-01\n").unwrap();

        // no certificate yet: enrollment with the token, then the report
        let reporter = Reporter::new(&setup.config, "web-01").unwrap();
        reporter.record(&result("web-01", "k1"), None, None).unwrap();
        reporter.flush().await.unwrap();
        {
            let calls = setup.calls.lock().unwrap();
            assert_eq!((calls.enrolled, calls.renewed, calls.reported.len()), (1, 0, 1));
        }

        // certificate inside the renewal window: renewed over mTLS
        // (the test CA issues certificates valid until 4096: use the widest window)
        setup.config.renew_before_sec = u64::MAX;
        let reporter = Reporter::new(&setup.config, "web-01").unwrap();
        reporter.record(&result("web-01", "k2"), None, None).unwrap();
        reporter.flush().await.unwrap();
        let calls = setup.calls.lock().unwrap();
        assert_eq!((calls.enrolled, calls.renewed, calls.reported.len()), (1, 1, 2));
        drop(calls);
        let _ = std::fs::remove_dir_all(&setup.dir);
    }

    #[tokio::test]
    async fn a_pass_streams_its_live_view_and_reports_its_log() {
        use regent_sdk::ExpectedState;
        use regent_sdk::hosts::handlers::{ConnectionMethod, TargetUser};
        use regent_sdk::hosts::managed_host::ManagedHostBuilder;
        use regent_sdk::task::RegentTask;

        let setup = setup("live", TlsMode::ExternalCa).await;
        let (cert, key) = setup.ca.leaf("web-01");
        std::fs::create_dir_all(setup.dir.join("tls")).unwrap();
        std::fs::write(&setup.config.cert_path, cert).unwrap();
        std::fs::write(&setup.config.key_path, key).unwrap();
        let reporter = Reporter::new(&setup.config, "web-01").unwrap();

        let mut builder = ManagedHostBuilder::new(
            "web-01",
            "localhost",
            Some(ConnectionMethod::Localhost(TargetUser::current_user())),
        );
        builder.set_host_lock(None);
        let state = ExpectedState::from_raw_yaml(
            "Attributes:\n  - Name: echo\n    Privilege: !None\n    Detail: !Command\n      Cmd: \"echo hello\"\n",
        )
        .unwrap();
        let mut task = RegentTask::from(builder, state, Job::Assess);
        let (sink, live) = reporter.start_live();
        task.enable_live(Some(sink));
        let result = task.run(None).await;
        task.disable_live();
        let log = live.finish().await;
        assert!(!log.updates().is_empty());
        reporter.record(&result, None, Some(&log)).unwrap();
        reporter.flush().await.unwrap();

        let calls = setup.calls.lock().unwrap();
        assert_eq!(
            calls.live.len(),
            log.updates().len(),
            "every update streamed"
        );
        assert!(calls.live.iter().all(|(host, _)| host == "web-01"));
        assert_eq!(calls.run_logs, vec![log.updates().len()]);
        drop(calls);
        let _ = std::fs::remove_dir_all(&setup.dir);
    }
}
