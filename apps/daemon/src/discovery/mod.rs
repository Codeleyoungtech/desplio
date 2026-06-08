use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tracing::{info, warn};

#[allow(dead_code)]
pub struct DiscoveryHandle {
    daemon: ServiceDaemon,
    fullname: String,
}

pub fn spawn_mdns_broadcaster(
    port: u16,
    shutdown: Arc<AtomicBool>,
) -> Result<Option<DiscoveryHandle>, Box<dyn std::error::Error>> {
    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            warn!("Failed to create mDNS daemon: {}", e);
            return Ok(None);
        }
    };

    let service_type = "_desplio._tcp.local.";
    let hostname = std::env::var("HOSTNAME")
        .unwrap_or_else(|_| gethostname::gethostname().to_string_lossy().to_string());
    let instance_name = format!("{} Desplio Host", hostname);

    let properties = vec![("version", "0.1.0")];

    let service_info = ServiceInfo::new(
        service_type,
        &instance_name,
        &format!("{}.local.", hostname.to_lowercase()),
        "",
        port,
        &properties[..],
    )
    .expect("valid service info");

    // The mdns_sd crate automatically publishes on all interfaces if no addresses are explicitly specified,
    // but we can just register the service.
    
    let fullname = service_info.get_fullname().to_string();

    match daemon.register(service_info) {
        Ok(_) => {
            info!("mDNS discovery active: advertised as {} on port {}", fullname, port);
        }
        Err(e) => {
            warn!("Failed to register mDNS service: {}", e);
            return Ok(None);
        }
    }

    let handle = DiscoveryHandle {
        daemon: daemon.clone(),
        fullname: fullname.clone(),
    };

    // Spawn a thread to unregister on shutdown
    let fullname_clone = fullname.clone();
    thread::spawn(move || {
        while !shutdown.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(500));
        }
        info!("Unregistering mDNS service {}", fullname_clone);
        let _ = daemon.unregister(&fullname_clone);
    });

    Ok(Some(handle))
}
