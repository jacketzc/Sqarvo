//! 由 `proto/` 生成 gRPC 代码。
//!
//! 生成过程不依赖系统安装的 `protoc`：
//!
//! 1. 使用 `protoc-bin-vendored` 提供的 protoc 生成 `FileDescriptorSet`；
//! 2. 把描述符交给 `tonic-prost-build` 生成 Rust 代码。
//!
//! 这样既避免了跨平台差异，也避免在构建脚本中修改进程级环境变量
//! （`std::env::set_var` 在 Rust 2024 中是 unsafe 的）。
//!
//! 生成文件位于 `OUT_DIR`，**不得手工提交或修改**。

use std::path::{Path, PathBuf};
use std::process::Command;

use prost::Message;
use prost_types::FileDescriptorSet;

fn main() {
    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR 未设置"));
    let proto_root = manifest_dir
        .join("../../../proto")
        .canonicalize()
        .expect("proto 目录不存在");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR 未设置"));

    let mut protos = Vec::new();
    collect_protos(&proto_root, &mut protos);
    protos.sort();
    assert!(!protos.is_empty(), "proto 目录中没有 .proto 文件");

    for proto in &protos {
        println!("cargo:rerun-if-changed={}", proto.display());
    }

    let descriptor_path = out_dir.join("sqarvo-descriptor.bin");
    let protoc_path = protoc_bin_vendored::protoc_bin_path().expect("无法定位内置 protoc");
    let mut command = Command::new(protoc_path);
    command
        .current_dir(&proto_root)
        .arg("--include_imports")
        .arg("--include_source_info")
        .arg("--descriptor_set_out")
        .arg(&descriptor_path);
    for proto in &protos {
        let relative = proto
            .strip_prefix(&proto_root)
            .expect("proto 路径不在 proto 根目录内");
        command.arg(relative);
    }
    let output = command.output().expect("无法执行 protoc");
    assert!(
        output.status.success(),
        "protoc 失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bytes = std::fs::read(&descriptor_path).expect("无法读取描述符文件");
    let file_descriptor_set =
        FileDescriptorSet::decode(bytes.as_slice()).expect("无法解析 FileDescriptorSet");

    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        // 客户端不生成 `connect(dst)` 便捷构造：ConnectionService 的 RPC 名称
        // `Connect` 会和该方法同名，生成代码无法编译。前端一律通过自定义
        // 连接器（Unix socket / 回环 TCP）构造 Channel。
        .build_transport(false)
        .compile_fds(file_descriptor_set)
        .expect("gRPC 代码生成失败");
}

/// 递归收集 `.proto` 文件，排序在调用处完成。
fn collect_protos(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("读取目录 {} 失败: {error}", dir.display()));
    for entry in entries {
        let entry = entry.expect("读取 proto 目录项失败");
        let path = entry.path();
        if path.is_dir() {
            collect_protos(&path, out);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "proto")
        {
            out.push(path);
        }
    }
}
