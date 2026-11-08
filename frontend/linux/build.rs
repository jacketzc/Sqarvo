//! 由 `../../proto` 生成前端使用的 gRPC 客户端代码。
//!
//! 与后端 `backend/crates/rpc-server/build.rs` 采用同一策略：
//!
//! 1. 用 `protoc-bin-vendored` 内置的 protoc 生成 `FileDescriptorSet`，
//!    因此不依赖系统安装的 protoc，也不需要修改进程环境变量；
//! 2. 把描述符交给 `tonic-prost-build` 生成 Rust 代码；
//! 3. 生成文件只写入 `OUT_DIR`，**不得手工提交或修改**。
//!
//! 只生成客户端代码（`build_server(false)`），并关闭 `build_transport(true)`
//! 的便捷构造：前端的 Channel 需要用自定义连接器（Unix socket）手工建立，
//! 见 `src/backend/client.rs`。

use std::path::{Path, PathBuf};
use std::process::Command;

use prost::Message;
use prost_types::FileDescriptorSet;

fn main() {
    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR 未设置"));
    let proto_root = manifest_dir
        .join("../../proto")
        .canonicalize()
        .expect("proto 目录不存在：frontend/linux 期望位于仓库的 frontend/linux 下");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR 未设置"));

    let mut protos = Vec::new();
    collect_protos(&proto_root, &mut protos);
    protos.sort();
    assert!(!protos.is_empty(), "proto 目录中没有 .proto 文件");

    // 契约变化必须触发重新生成。
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", proto_root.display());
    for proto in &protos {
        println!("cargo:rerun-if-changed={}", proto.display());
    }

    let descriptor_path = out_dir.join("sqarvo-frontend-descriptor.bin");
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
        .build_server(false)
        // 前端不使用 `connect(dst)` 便捷构造：`ConnectionService.Connect` 会与
        // 生成的方法同名，而且本地 IPC 需要自定义连接器。
        .build_transport(false)
        .compile_fds(file_descriptor_set)
        .expect("gRPC 客户端代码生成失败");

    compile_icon_resources(&manifest_dir, &out_dir);
}

/// 把项目自有符号图标编成 `.gresource` 写进 `OUT_DIR`。
///
/// 图标只有一份来源（`resources/icons/*.svg`），由 [`crate::icons`] 在运行时注册成
/// `GtkIconTheme` 的资源搜索路径；`.gresource` 通过 `include_bytes!` 进二进制，
/// 安装包里不会多出需要维护的图标文件。
fn compile_icon_resources(manifest_dir: &Path, out_dir: &Path) {
    let source_dir = manifest_dir.join("resources");
    let manifest = source_dir.join("icons.gresource.xml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed={}", source_dir.display());

    let target = out_dir.join("sqarvo-icons.gresource");
    let output = Command::new("glib-compile-resources")
        .arg("--sourcedir")
        .arg(&source_dir)
        .arg("--target")
        .arg(&target)
        .arg(&manifest)
        .output()
        .unwrap_or_else(|error| {
            panic!("无法执行 glib-compile-resources（需要 glib-2.0 开发包）：{error}")
        });
    assert!(
        output.status.success(),
        "编译图标资源失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
