//! 公共契约导出；--check只比对，不写文件。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let check = std::env::args().skip(1).any(|arg| arg == "--check");
    for (name, content) in agentnotify_core::contracts::generated_artifacts() {
        let path = root.join(name);
        if check {
            if std::fs::read_to_string(&path)? != content {
                return Err(format!("generated contract drift: {name}").into());
            }
        } else {
            std::fs::create_dir_all(path.parent().ok_or("missing parent")?)?;
            std::fs::write(path, content)?;
        }
    }
    Ok(())
}
