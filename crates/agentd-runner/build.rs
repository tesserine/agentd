fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_else(|_| "unknown".to_string());
    if target_os != "linux" {
        println!(
            "cargo:error=agentd-runner requires target_os = \"linux\"; got {target_os}. \
this failure is intentional because the runner depends on Linux runtime primitives"
        );
        std::process::exit(1);
    }
}
