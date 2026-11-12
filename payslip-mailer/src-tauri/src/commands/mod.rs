//! IPC 边界（设计文档 ch.4 LAYER 2）
//!
//! 每个 Command 负责：参数校验 → 调用 domain/infra → 组装 DTO 返回。
//! 返回值必须是「渲染后的视图模型」，不含密码、凭据与附件路径。

pub mod app;
pub mod auth;
pub mod backup;
pub mod batch;
pub mod export;
pub mod fields;
pub mod import;
pub mod members;
pub mod periods;
pub mod records;
pub mod sendlog;
pub mod settings;
pub mod templates;
pub mod verify;
