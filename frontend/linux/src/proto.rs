//! 由 `proto/` 生成的 gRPC 类型（仅客户端）。
//!
//! 目录结构必须与 Protobuf package 对应，因为生成代码中的跨 package 引用形如
//! `super::super::common::v1::Foo`。
//!
//! **禁止手工编辑生成代码。** 生成过程见 `build.rs`；契约以 `proto/` 为准。

/// `sqarvo.common.v1`
pub mod common {
    /// `sqarvo.common.v1`
    #[allow(missing_docs, dead_code, clippy::all, clippy::pedantic)]
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
