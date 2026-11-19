//! CLI validation fails before opening credentials or reading MCP configuration.

use std::net::TcpListener;
use std::process::Command;

const MCP: &str = env!("CARGO_BIN_EXE_sqarvo-mcp");

#[test]
fn rejects_non_loopback_hosts_and_zero_port() {
    for args in [
        ["--host", "0.0.0.0"],
        ["--host", "::"],
        ["--host", "192.0.2.1"],
        ["--port", "0"],
    ] {
        let output = Command::new(MCP).arg("serve").args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(output.stdout, [] as [u8; 0]);
    }
}

#[test]
fn reports_port_in_use_without_touching_the_existing_listener() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let output = Command::new(MCP)
        .args(["serve", "--port", &port])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mcp.http.port_in_use"));
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert!(TcpListener::bind(("127.0.0.1", port.parse::<u16>().unwrap())).is_err());
}
