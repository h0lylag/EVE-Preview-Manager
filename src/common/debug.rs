use std::process::Command;
use tracing::info;

/// Log system information for debugging purposes
pub fn log_system_info() {
    info!("=== System Information ===");

    // Kernel Version
    if let Ok(kernel) = get_command_output("uname", &["-sr"]) {
        info!("Kernel: {}", kernel);
    }

    // OS / Distribution
    if let Ok(os_release) = std::fs::read_to_string("/etc/os-release") {
        for line in os_release.lines() {
            if line.starts_with("PRETTY_NAME=") {
                let name = line.trim_start_matches("PRETTY_NAME=").trim_matches('"');
                info!("OS: {}", name);
                break;
            }
        }
    }

    // Desktop Environment / Window Manager hints
    if let Ok(session) = std::env::var("XDG_SESSION_TYPE") {
        info!("Session Type: {}", session);
    }
    if let Ok(desktop) = std::env::var("XDG_CURRENT_DESKTOP") {
        info!("Desktop Environment: {}", desktop);
    }

    // CPU Arch
    if let Ok(arch) = get_command_output("uname", &["-m"]) {
        info!("Architecture: {}", arch);
    }

    // Memory (Total/Available from /proc/meminfo)
    if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
        let parse_memory_gb = |line: &str, prefix: &str| -> Option<f64> {
            let value_str = line.strip_prefix(prefix)?.trim();
            // Expected format: "16386456 kB"
            if let Some(kb_str) = value_str.split_whitespace().next() {
                if let Ok(kb) = kb_str.parse::<u64>() {
                    return Some(kb as f64 / 1024.0 / 1024.0);
                }
            }
            None
        };

        let mut total_gb = None;
        let mut available_gb = None;

        for line in meminfo.lines() {
            if line.starts_with("MemTotal:") {
                total_gb = parse_memory_gb(line, "MemTotal:");
            } else if line.starts_with("MemAvailable:") {
                available_gb = parse_memory_gb(line, "MemAvailable:");
            }
        }

        if let (Some(total), Some(avail)) = (total_gb, available_gb) {
            info!(
                "Memory: {:.2} GB (Total), {:.2} GB (Available)",
                total, avail
            );
        } else {
            // Fallback for unexpected formats
            // info!("Memory information found but could not be parsed");
        }
    }

    // Loaded Graphics Drivers (from /proc/modules)
    if let Ok(modules) = std::fs::read_to_string("/proc/modules") {
        let mut drivers = Vec::new();
        for line in modules.lines() {
            let module_name = line.split_whitespace().next().unwrap_or("");
            if matches!(
                module_name,
                "nvidia" | "amdgpu" | "i915" | "nouveau" | "radeon"
            ) {
                drivers.push(module_name);
            }
        }
        if !drivers.is_empty() {
            info!("Graphics Drivers: {}", drivers.join(", "));
        }
    }

    info!("==========================");
}

fn get_command_output(cmd: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(cmd).args(args).output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
