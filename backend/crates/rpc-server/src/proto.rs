//! 由 `proto/` 生成的 gRPC 类型。
//!
//! 目录结构必须与 Protobuf package 对应，因为生成代码中的跨 package 引用
//! 形如 `super::super::common::v1::Foo`。
//!
//! **禁止手工编辑生成代码。** 如需调整 API，请修改 `proto/` 并重新构建。
//! 生成代码的文档来自 `.proto` 注释；此处统一放宽 `missing_docs`
//! （协议文档以 Protobuf 注释为准）。

/// `sqarvo.common.v1`
pub mod common {
    /// `sqarvo.common.v1`
    #[allow(missing_docs, clippy::all, clippy::pedantic)]
    pub mod v1 {
        tonic::include_proto!("sqarvo.common.v1");
    }
}

/// `sqarvo.services.v1`
pub mod services {
    /// `sqarvo.services.v1`
    #[allow(missing_docs, clippy::all, clippy::pedantic)]
    pub mod v1 {
        tonic::include_proto!("sqarvo.services.v1");
    }
}
