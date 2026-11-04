//! 内置驱动注册表。
//!
//! 第一阶段不引入动态库插件 ABI：所有驱动以 Rust workspace crate 形式
//! 编译进后端，由 [`DriverRegistry`] 在启动时注册。
//!
//! 当前仓库尚未实现任何数据库驱动；`with_builtin_drivers` 因此返回空注册表。
//! 未注册的驱动 ID 会以 `driver.not_found` 明确失败，不会伪装成可用。
//!
//! `sqarvo-driver-sqlite` 已存在（连接/只读/关闭），但在执行与游标能力完成
//! （M2 #10–#12）之前不注册，避免 `ListDrivers` 提前声明可用能力。

#![warn(missing_docs)]

pub mod registry;

pub use registry::DriverRegistry;
