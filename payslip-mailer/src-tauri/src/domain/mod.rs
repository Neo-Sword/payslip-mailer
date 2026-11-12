//! 领域层（设计文档 ch.4 LAYER 3）
//!
//! 放实体、状态机、校验规则与幂等键 —— 纯逻辑，不碰数据库、SMTP、文件系统，
//! 因此可以完整单测。

pub mod admin;
pub mod auth;
pub mod backup;
pub mod field;
pub mod formula;
pub mod import;
pub mod member;
pub mod period;
pub mod ratelimit;
pub mod record;
pub mod retry;
pub mod sheet;
pub mod template;
