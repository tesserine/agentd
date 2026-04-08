use std::process::Command;

#[test]
fn binary_installs_tracing_before_running() {
    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .env("AGENTD_LOG_FORMAT", "text")
        .output()
        .expect("agentd binary should run");

    assert!(
        output.status.success(),
        "agentd binary failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be valid UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be valid UTF-8");

    assert!(
        stdout.contains("agentd v0.1.0"),
        "expected version banner, got: {stdout}"
    );
    assert!(
        stderr.contains("\"event\":\"agentd.logging_format_invalid\""),
        "expected tracing bootstrap warning, got: {stderr}"
    );
}
