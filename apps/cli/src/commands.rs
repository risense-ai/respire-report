//! 冻结命令语法；此层不理解厂商Hook字段，不注册真实Hook或读取凭据。
use crate::output;
use agentnotify_core::contracts::InputError;
use std::path::PathBuf;
use uuid::Uuid;
pub const HELP: &str = "AgentNotify\n\n用法：agentnotify [--data-dir <absolute-dir>] <command>\n\n  doctor --json\n  integration inspect --json\n  integration plan --request <json-file>\n  integration apply --plan <json-file>\n  integration verify --operation-id <id>\n  integration rollback --operation-id <id>\n  hook <adapter> --instance-id <uuid>  (stdin)\n  emit --stdin\n  service install|start|stop|status|uninstall\n  update verify|prepare --manifest <json-file>\n  update status|apply|rollback|resume\n  uninstall --plan|--apply  (保留数据/凭据/Hook)\n  notifications pause|resume\n  agent-bundle create --task <json-file> --out <dir>\n  test --dry-run\n  test --send\n\n--data-dir 仅接受当前用户安全绝对目录，不会绕过权限。\n退出码：0成功，2输入错误，3需要用户动作，4运行失败。Hook遵从宿主非阻塞约定。\n尚未实现的后端明确返回unavailable，不代表已通知。";
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Desktop,
    Update { action: String, manifest: Option<PathBuf> },
    Uninstall(bool),
    UninstallHelper(Uuid),
    LaunchDaemon,
    Pause(bool),
    Doctor,
    ObserverEnable(Uuid),
    Inspect,
    MigrationInspect(PathBuf),
    MigrationSecret,
    MigrationPrepare(PathBuf),
    Plan(PathBuf),
    Apply(PathBuf),
    Verify(String),
    Rollback(String),
    Hook { adapter: String, instance: Uuid },
    Emit,
    Service(String),
    Bundle { task: PathBuf, out: PathBuf },
    DryRun,
    Send,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Invocation {
    pub data_dir: Option<PathBuf>,
    pub command: Command,
}
/// 参数解析与错误路由共用全局选项剥离，重复选项也先识别真实命令。
fn split_global_options(args: &[String]) -> (Vec<String>, Result<Option<PathBuf>, InputError>) {
    let mut positional = Vec::new();
    let mut data_dir = None;
    let mut error = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] != "--data-dir" {
            positional.push(args[index].clone());
            index += 1;
            continue;
        }
        if index + 1 >= args.len() {
            error.get_or_insert_with(|| InputError::new("invalid_arguments"));
            index += 1;
            continue;
        }
        if data_dir.is_some() {
            error.get_or_insert_with(|| InputError::new("invalid_arguments"));
        }
        let dir = PathBuf::from(&args[index + 1]);
        if !dir.is_absolute() {
            error.get_or_insert_with(|| InputError::new("unsafe_data_dir"));
        }
        data_dir = Some(dir);
        index += 2;
    }
    (
        positional,
        match error {
            Some(error) => Err(error),
            None => Ok(data_dir),
        },
    )
}
/// 即使全局参数无效，也按相同词法规则识别Hook；参数值叫hook不算命令。
pub fn is_hook_invocation(args: &[String]) -> bool {
    split_global_options(args)
        .0
        .first()
        .is_some_and(|command| command == "hook")
}
/// 严格参数解析，拒绝多余字段及未知命令；错误不回显任何实参。
pub fn parse(args: &[String]) -> Result<Invocation, InputError> {
    let (args, data_dir) = split_global_options(args);
    let data_dir = data_dir?;
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let command = match a.as_slice() {
        [] | ["--help"] | ["-h"] => Command::Help,
        ["update", "verify", "--manifest", path] | ["update", "prepare", "--manifest", path] => Command::Update { action: a[1].into(), manifest: Some(PathBuf::from(path)) },
        ["update", action] if matches!(*action, "status" | "apply" | "rollback" | "resume") => Command::Update { action: (*action).into(), manifest: None },
        ["uninstall-helper", id] => Command::UninstallHelper(Uuid::parse_str(id).map_err(|_|InputError::new("invalid_identity"))?),
        ["uninstall", "--plan"] => Command::Uninstall(false),
        ["uninstall", "--apply"] => Command::Uninstall(true),
        ["desktop"] => Command::Desktop,
        ["launch-daemon"] => Command::LaunchDaemon,
        ["notifications", "pause"] => Command::Pause(true),
        ["notifications", "resume"] => Command::Pause(false),
        ["doctor", "--json"] => Command::Doctor,
        ["observer", "enable", "codex", "--instance-id", id] => Command::ObserverEnable(Uuid::parse_str(id).map_err(|_|InputError::new("invalid_identity"))?),
        ["integration", "inspect", "--json"] => Command::Inspect,
        ["migration", "prepare", "--root", root] => Command::MigrationPrepare(PathBuf::from(root)),
        ["migration", "store-secret", "--stdin"] => Command::MigrationSecret,
        ["migration", "inspect", "--root", root] => Command::MigrationInspect(PathBuf::from(root)),
        ["integration", "plan", "--request", file] => Command::Plan(PathBuf::from(file)),
        ["integration", "apply", "--plan", file] => Command::Apply(PathBuf::from(file)),
        ["integration", "verify", "--operation-id", id] => {
            agentnotify_core::contracts::validate_identity(id)?;
            Command::Verify((*id).into())
        }
        ["integration", "rollback", "--operation-id", id] => {
            agentnotify_core::contracts::validate_identity(id)?;
            Command::Rollback((*id).into())
        }
        ["hook", adapter, "--instance-id", id] => {
            if !matches!(
                *adapter,
                "generic" | "codex" | "claude_code" | "claude-code"
            ) {
                return Err(InputError::new("unsupported_adapter"));
            }
            Command::Hook {
                adapter: (*adapter).into(),
                instance: Uuid::parse_str(id).map_err(|_| InputError::new("invalid_identity"))?,
            }
        }
        ["emit", "--stdin"] => Command::Emit,
        ["service", action]
            if matches!(
                *action,
                "install" | "start" | "stop" | "status" | "uninstall"
            ) =>
        {
            Command::Service((*action).into())
        }
        ["agent-bundle", "create", "--task", file, "--out", dir] => Command::Bundle {
            task: PathBuf::from(file),
            out: PathBuf::from(dir),
        },
        ["test", "--dry-run"] => Command::DryRun,
        ["test", "--send"] => Command::Send,
        _ => return Err(InputError::new("invalid_arguments")),
    };
    Ok(Invocation { data_dir, command })
}
/// 语法校验后分派受限宿主命令；未实现能力继续明确unavailable。
pub fn execute(invocation: Invocation) -> i32 {
    if let Command::UninstallHelper(id)=invocation.command {
        let paths=invocation.data_dir.as_ref().map(|p|agentnotify_platform::paths::PlatformPaths::override_data(p)).unwrap_or_else(agentnotify_platform::paths::PlatformPaths::discover);
        return match paths { Ok(paths)=>if crate::uninstall::finish(paths,id).is_ok(){0}else{4},Err(_)=>4 };
    }
    if matches!(invocation.command, Command::MigrationPrepare(_)) { return crate::migration_secret::prepare(invocation); }
    if matches!(invocation.command, Command::MigrationSecret) { return crate::migration_secret::execute(invocation); }
    if matches!(invocation.command, Command::Update { .. } | Command::Uninstall(_)) { return crate::update_commands::execute(invocation); }
    if let Command::Help = invocation.command {
        println!("{HELP}");
        return 0;
    }
    if matches!(invocation.command, Command::Bundle { .. }) {
        return crate::agent_bundle_commands::execute(invocation);
    }
    if matches!(invocation.command, Command::Inspect | Command::MigrationInspect(_) | Command::Plan(_) | Command::Apply(_) | Command::Verify(_) | Command::Rollback(_)) {
        return crate::integration_commands::execute(invocation);
    }
    if matches!(
        invocation.command,
        Command::Doctor
            | Command::ObserverEnable(_)
            | Command::Service(_)
            | Command::Emit
            | Command::Hook { .. }
            | Command::Desktop
            | Command::LaunchDaemon
            | Command::Pause(_)
    ) {
        return crate::host_commands::execute(invocation);
    }
    match invocation.command {
        Command::DryRun => output::report(
            3,
            "configuration_required",
            "预演未发送：邮箱、收件人、安全凭据及投递后端尚未配置。",
            Some(
                serde_json::json!({"sent":false,"dry_run":true,"missing":["sender","recipient","secure_credentials","transport"]}),
            ),
        ),
        _ => output::unavailable(),
    }
}
