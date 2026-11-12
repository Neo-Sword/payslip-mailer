// 发布版窗口下隐藏 macOS Dock 图标：这是一个常驻后台的工具型应用
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    payslip_mailer_lib::run()
}
