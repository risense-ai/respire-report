//! CLI输出只使用受控报告；宿主Hook不向stdout发送流程控制指令。
use agentnotify_core::contracts::CommandReport;
use serde_json::Value;
pub fn report(code: i32, error: &str, message: &str, data: Option<Value>) -> i32 {
    let result = CommandReport::failure(error, message, data);
    match serde_json::to_string(&result) {
        Ok(json) => println!("{json}"),
        Err(_) => {
            eprintln!("report_serialization_failed");
            return 4;
        }
    }
    code
}
pub fn unavailable() -> i32 {
    report(
        4,
        "unavailable",
        "此操作的后端尚未实现，未执行任何变更或发送。",
        None,
    )
}
