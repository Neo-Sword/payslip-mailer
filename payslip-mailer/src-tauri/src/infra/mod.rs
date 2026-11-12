//! 基础设施层（设计文档 ch.4 LAYER 4）：SQLCipher / Keychain / Excel / 模板 / SMTP / PDF。
//!
//! 这一层的定位是「副作用的出口」：上层不直接碰系统调用，
//! 便于把「数据不出本机」的承诺集中在少数几个可审计的地方。

pub mod admins;
pub mod backup;
pub mod batch;
pub mod db;
pub mod export;
pub mod fields;
pub mod import;
pub mod keychain;
pub mod mailer;
pub mod members;
pub mod password;
pub mod periods;
pub mod records;
pub mod sendlog;
pub mod sheet_schema;
pub mod templates;
pub mod verify;
pub mod wake;
pub mod wipe;
