//! 通过真实子进程验证终端策略操作，共享目录完全隔离。
#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn access(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sqarvo-mcp"))
        .args(["access"])
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("APPDATA", home.join("roaming"))
        .env("LOCALAPPDATA", home.join("local"))
        .stdin(Stdio::null())
        .output()
        .expect("启动 access 子进程")
}

fn status(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 状态")
}

#[test]
fn management_processes_share_policy_and_require_confirmation() {
    let directory = tempfile::tempdir().expect("隔离目录");
    let home = directory.path();
    let initial = status(access(home, &["status"]));
    assert!(initial.starts_with("enabled=false\nread_only=true\nprofiles=0\nrevision="));

    let no_confirmation = access(home, &["on"]);
    assert!(!no_confirmation.status.success());
    assert!(String::from_utf8_lossy(&no_confirmation.stderr).contains("--yes"));
    assert_eq!(no_confirmation.stdout, [] as [u8; 0]);
    assert_eq!(status(access(home, &["status"])), initial);

    let disabled = access(home, &["read-only", "off", "--yes"]);
    assert!(!disabled.status.success());
    assert!(String::from_utf8_lossy(&disabled.stderr).contains("全局未开启"));
    assert_eq!(status(access(home, &["read-only", "status"])), initial);

    let enabled = status(access(home, &["on", "--yes"]));
    assert!(enabled.starts_with("enabled=true\nread_only=true\n"));
    assert_eq!(status(access(home, &["status"])), enabled);

    assert!(!access(home, &["read-only", "off"]).status.success());
    assert_eq!(status(access(home, &["status"])), enabled);
    let read_only_off = status(access(home, &["read-only", "off", "--yes"]));
    assert!(read_only_off.starts_with("enabled=true\nread_only=false\n"));
    assert!(
        status(access(home, &["read-only", "on"])).starts_with("enabled=true\nread_only=true\n")
    );

    status(access(home, &["read-only", "off", "--yes"]));
    let revoked = status(access(home, &["off"]));
    assert!(revoked.starts_with("enabled=false\nread_only=true\n"));
    assert_eq!(status(access(home, &["status"])), revoked);
}
