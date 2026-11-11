//! Running as a Windows service (`--windows-service`, set by the MSI's service
//! registration): the Service Control Manager starts the usual service loop
//! on its own thread; Stop and Shutdown end the process, like SIGTERM under
//! systemd (outcomes are spooled on disk, nothing is lost).

use std::ffi::OsString;
use std::time::Duration;

use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

const SERVICE_NAME: &str = "regops";

define_windows_service!(ffi_service_main, service_main);

/// Hand the process over to the SCM; never returns.
pub fn run() -> ! {
    if let Err(e) = service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
        eprintln!("regops: cannot start as a Windows service: {e}");
        std::process::exit(1);
    }
    std::process::exit(0)
}

fn status(state: ServiceState) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::NO_ERROR,
        checkpoint: 0,
        wait_hint: Duration::from_secs(10),
        process_id: None,
    }
}

fn service_main(_arguments: Vec<OsString>) {
    let handle: std::sync::Arc<
        std::sync::Mutex<Option<service_control_handler::ServiceStatusHandle>>,
    > = std::sync::Arc::new(std::sync::Mutex::new(None));
    let for_handler = handle.clone();
    let registered =
        service_control_handler::register(SERVICE_NAME, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                if let Some(handle) = *for_handler.lock().expect("handle") {
                    let _ = handle.set_service_status(status(ServiceState::Stopped));
                }
                std::process::exit(0);
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        });
    let Ok(status_handle) = registered else {
        return;
    };
    *handle.lock().expect("handle") = Some(status_handle);
    let _ = status_handle.set_service_status(status(ServiceState::Running));
    // the usual entry point, arguments included (--config-path ...)
    tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(crate::async_main());
}
