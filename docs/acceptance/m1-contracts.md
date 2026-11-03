# M1 公共契约实现证据

状态：实现交付待独立核验。此记录不授予 M1 独立验收通过。

范围为任务 02：工具中立事件、共享证据、生命周期/接收单元、配置、隐私边界、严格 CLI 协议和自动生成契约。未实现队列、策略、看门狗计时器、SMTP、系统服务或客户端适配器业务。

## 实际验证

所有命令在项目根目录执行，通过当前进程 PATH/CARGO_HOME/RUSTUP_HOME 使用 M0 工具链；没有修改全局环境。

| 命令 | 返回码 | 实际结果 |
| --- | --- | --- |
| `pwsh -File scripts/dev.ps1 test -p agentnotify-core --test contracts`（初始 RED） | 101 | 公共模块/依赖尚不存在，不能编译；不是产品通过 |
| `pwsh -File scripts/dev.ps1 test -p agentnotify-cli --test protocol`（初始 RED） | 101 | M0包名为agentnotify，固定agentnotify-cli尚不存在 |
| `pwsh -File scripts/dev.ps1 test -p agentnotify-cli --test protocol --locked`（补充 RED） | 101 | malformed_hook_never_blocks_host_or_writes_stdout：实际2，预期0 |
| `pwsh -File scripts/dev.ps1 test -p agentnotify-core --test contracts --locked`（补充 RED） | 101 | envelope_metadata_and_reception_time_are_checked：接收时间不一致却返回Ok |
| `pwsh -File scripts/dev.ps1 test -p agentnotify-core -p agentnotify-cli --locked`（最终 GREEN） | 0 | 核心15、CLI9测试通过 |
| `pwsh -File scripts/dev.ps1 test --workspace --locked` | 0 | 24项M1测试及4项既有M0探针通过；2个进程辅助入口默认ignored并由主探针调用 |
| `pwsh -File scripts/dev.ps1 fmt --all -- --check` | 0 | Rust格式检查 |
| `pwsh -File scripts/dev.ps1 run -p agentnotify-core --example export_contracts --locked -- --check` | 0 | 两份Schema与TS与Rust确定性导出结果一致 |
| `pwsh -File scripts/dev.ps1 run -p agentnotify-cli --locked -- --help` | 0 | 展示完整冻结命令，说明data-dir不绕过权限 |
| `pwsh -File scripts/dev.ps1 run -p agentnotify-cli --locked -- test --dry-run` | 3 | configuration_required；sent=false，列出sender/recipient/secure_credentials/transport缺项 |

初期直接在PowerShell调用脚本会吞掉分隔用的`--`，导致一次导出检查报unexpected argument、一次help显示Cargo帮助。改为`pwsh -File`后重新取得真实导出检查和产品帮助，前次不计通过。一次实现编译因PrivacyError字符串类型不符、一次导出编译因serde_json::Value无TS实现失败，均修正后才取得上述GREEN。

原始最终输出位于被忽略的`test-artifacts/m1/{workspace-tests,help,dry-run}.txt`；这里只记录可审查的受控结论，不将临时过程文件入库。没有真实SMTP投递、用户客户端Hook变更或GUI验证。

## 契约与使用边界

- `core::event`是通知、actor、SourceEvidence、LifecycleSignal、AdapterOutput、IngressEnvelopeV1、ActorKey/TurnKey/ActivityKey的唯一类型来源。SourceEvidence只在AdapterOutput保存一次，纯生命周期也拥有共同证据。无序号时显式诊断order_unknown。
- `core::contracts::parse_generic`只认识中立字段；外部可信分类/本机接收时间/计时字段均拒绝。实例与IngressContext一致，来源固定caller_reported/unknown；task_completed仅允许explicit_caller_report。内部明确任务事件可使用explicit_task_event。显示分别为“本轮处理结束”“调用方报告完成”“收到明确任务完成事件”。
- 原始读取至多1MiB+1个检测字节；身份256、标签512、无正文元数据8192字节，按UTF-8字节校验。正文隐私关闭先清除；开启每项至多16384字节、不破坏字符边界并记录截断标记。安全密钥可用性由可信本机IngressContext注入，不可通过JSON字段开启；缺密钥返回content_key_unavailable。SanitizedEvent本身不实现Serialize，保留正文仍须后续存储任务单独AEAD封装。
- 配置只含非敏感偏好，版本1，未知字段拒绝。默认值遵从设计；同目录tempfile写入、sync_all后原子替换。未知版本或无效配置不会覆盖原文件。当前timezone仅system；IANA时区策略留任务04。watchdog模式、阈值/能力约束已有校验，不代表看门狗计时器已实现。
- `schemas/event-v1.schema.json`根为内部IngressEnvelopeV1，含NormalizedEventV1、LifecycleSignal和共同来源证据定义；generic入口是同一业务模型的受限外部投影，需要运行时保留字段/语义检查。Schema不替代UTF-8字节预算、跨字段完成语义及来源授权校验。`config-v1`根为ConfigV1。两份Schema和UI类型全部通过Rust derive生成，不手改文件；生成命令见上表。
- CLI package对齐为agentnotify-cli，二进制仍agentnotify。通用退出码0/2/3/4固定。后端命令均返回ok=false/unavailable，不代表持久化或发送。Hook（包括参数错误）退出0、stdout空、stderr为脱敏报告，不产生blocker/continue指令。emit合法输入仍返回4/unavailable，不能当成可靠入队确认。
- `--data-dir`的相对/越界目录返回2；当前用户目录尚无平台ACL证明时返回3/secure_directory_unavailable。不会创建目录或将home前缀误当安全证明。平台安全目录核验后由后续任务接通；本轮不谎报已支持自定义目录。
- core没有adapters/platform依赖，没有识别原生Hook字段、扫描客户端数据库或读取真实凭据。锁定chrono=0.4.42、schemars=1.0.4、ts-rs=11.0.1；序号、generation及经过秒数保持Rust u64 / TS number，serde双向边界和显式校验统一拒绝超过9007199254740991，Schema同步maximum。

未验证：Tauri/Vue运行和TS编译（当前桌面脚手架没有TS编译器）、Windows干净稳定系统、macOS/Linux实机、系统安全目录ACL后端、正文AEAD持久化、真正入队/投递。完整使用场景留后续任务，不以安全占位命令或零测试模块冒充完成业务。

收尾Git检查曾发现ts-rs生成doc注释前有尾随空格；生成器统一清理后重新导出，核心15项、生成漂移和格式检查重新返回0。生成文件未经手改。


## 独立验收FAIL后的任务02整改

整改前staged tree：`f3551864d2664ef08c9cf38048ce344fb1a295d6`。按主控分派原ownership实施，不自授独立验收通过，等待复核；没有启动子代理、commit/push或扩大到真实后端。

### 对应整改

1. **公共接口/DTO**：Ingress::accept改为`Result<EnqueueOutcome, QueueError>`，新增Queue::ingest同签名；补齐QueueError/QueueErrorCode、PolicyDecision/Policy、SendStage/DeliveryOutcome、HealthReport/DaemonStatus/CredentialStatus/QueueCounts/IntegrationHealth/HealthIssue。Policy显式接受`&SourceEvidence`；DeliveryOutcome保留Accepted/Retryable/AuthBlocked/PermanentFailure/Uncertain，SendStage保留五个阶段。CredentialStore、Transport为可注入中立trait；CredentialRef随机引用，CredentialHandle/SecretInput/PreparedMail不实现Serialize/Debug，SecretInput/句柄也不Clone，秘密缓冲区用Zeroizing释放清零。仅增加直接依赖zeroize=1.9.1（版本此前已在锁文件中），没有安全存储或发送fallback。Rust公共DTO同步导入event-v1 Schema的$defs和生成TS；秘密及仅内存输入不会导出。
2. **接入状态转换**：`IntegrationStatus::transition_to(next)`统一校验，非法边返回code=invalid_integration_transition和受控from/to。相同状态允许幂等观察；Configured只能取得探针证据后进ProbePassed，之后真实事件可进LiveVerified；TrustPending/ReloadPending需用户动作完成回Configured（TrustPending也可进ReloadPending），不能跳过待办直进ProbePassed/LiveVerified。UnsupportedVersion/ResearchPending必须重新Detected后才能配置；Degraded恢复后需重新配置、探针及真实事件。重新检测/卸载可退Detected/NotDetected，重配清理旧证据退Configured。转换校验不执行真实探针，也不替调用方制造证据。
3. **Hook错误路由**：参数解析与错误路由共用split_global_options，不再只识别一个前缀；重复--data-dir即使报输入错误，也在剥离全部全局参数后识别真实命令。所有被识别为Hook的错误exit0、stdout空、stderr脱敏报告。目录值名为hook不冒充宿主命令。已覆盖两组前缀、前后混排、Hook后参数等定点排列。
4. **整数精度**：Rust保持计划Option<u64>/Generation(u64)，不修改协议为字符串；MAX_SAFE_INTEGER=9007199254740991。序号、generation、watchdog elapsed_seconds在serde输入和输出均检查，IngressEnvelope::validate和事件计时校验也检查，超限integer_out_of_range。Schema同步maximum，TS仍number，但超过上限不能通过协议输入或序列化落盘；最大安全值精确往返。QueueCounts为u32，天然处于安全整数范围。

### 实际RED命令、输出与结果

所有命令从AgentNotify根执行，沿用当前进程CARGO_HOME/RUSTUP_HOME/PATH；没有修改全局工具链。以下失败均在对应生产修复之前观察：

- `pwsh -NoProfile -File scripts/dev.ps1 test -p agentnotify-cli --test protocol --locked repeated_global_options_never_route_hook_errors_to_stdout`：退出101。实际输出：`running 1 test`、`repeated_global_options_never_route_hook_errors_to_stdout ... FAILED`，断言`left: 2 / right: 0`；`0 passed; 1 failed; 9 filtered out`。
- `pwsh -NoProfile -File scripts/dev.ps1 test -p agentnotify-core --test contracts --locked json_integer_precision_boundary_is_enforced_on_input_and_output`：退出101。实际输出：`json_integer_precision_boundary_is_enforced_on_input_and_output ... FAILED`，`assertion failed: envelope.validate().is_err()`；`0 passed; 1 failed; 15 filtered out`。
- `pwsh -NoProfile -File scripts/dev.ps1 test -p agentnotify-core --test contracts --locked`：退出101。新增共享类型和转换测试后编译失败，首项`cannot find type QueueError`，随后Queue/QueueErrorCode/PolicyDecision/SendStage/DeliveryOutcome缺失，合计18个编译错误。这是缺失接口的RED，不宣称运行时失败。
- 同一完整core命令在新增安全句柄/传输契约测试、实现前再次退出101：`cannot find trait CredentialStore`，CredentialRef/CredentialHandle/CredentialError/SecretInput/Transport/PreparedMail等缺失，合计23个编译错误。此时此前共享DTO已能编译。

### 实际GREEN命令、输出与结果

- `pwsh -NoProfile -File scripts/dev.ps1 fmt --all`：0。
- `pwsh -NoProfile -File scripts/dev.ps1 run -p agentnotify-core --example export_contracts --offline`：0。更新本crate锁定依赖并重新生成全部契约，未下载/升级既有zeroize版本。
- `pwsh -NoProfile -File scripts/dev.ps1 test -p agentnotify-core --test contracts --locked`：0。实际19项全部通过：原15项及json_integer_precision_boundary_is_enforced_on_input_and_output、shared_policy_delivery_and_ingress_contracts_are_importable_and_typed、integration_transitions_require_configuration_and_evidence_stages、credential_transport_contracts_do_not_require_platform_dependencies。输出终行`test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s`。
- `pwsh -NoProfile -File scripts/dev.ps1 test -p agentnotify-cli --test protocol --locked`：0，实际完整输出附后。
- `pwsh -NoProfile -File scripts/dev.ps1 run -p agentnotify-core --example export_contracts --locked -- --check`：0，实际输出附后。
- `pwsh -NoProfile -File scripts/dev.ps1 fmt --all -- --check`：0，无输出。

本整改按主控要求未重复M0或完整workspace，也未运行真实客户端/SMTP/GUI。合同测试中的NoCredentials/NoTransport仅测试编译可注入边界，不计生产凭据或投递通过。正文是否有可用本机密钥与BodyProtector后续加密持久化仍分别验收。

CLI协议GREEN原始输出：

```text
   Compiling agentnotify-cli v0.1.0 (<workspace>\apps\cli)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.10s
     Running tests\protocol.rs (target\debug\deps\protocol-ae48ce140348f493.exe)

running 10 tests
test dry_run_reports_missing_config_without_sending ... ok
test emit_is_bounded_and_not_acknowledged_without_ingress ... ok
test help_is_real_and_no_password_option ... ok
test hook_is_nonblocking_and_emits_no_host_directives ... ok
test emit_read_budget_and_valid_input_do_not_leak_bodies ... ok
test malformed_hook_never_blocks_host_or_writes_stdout ... ok
test invalid_inputs_are_structured_and_redacted ... ok
test repeated_global_options_never_route_hook_errors_to_stdout ... ok
test unavailable_never_reports_success ... ok
test all_frozen_commands_parse_before_unavailable_backend ... ok

test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.66s


```

导出检查原始输出：

```text
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.18s
     Running `target\debug\examples\export_contracts.exe --check`

```


整改最终Git结果：相对f3551864d2664ef08c9cf38048ce344fb1a295d6仅11个自有路径修改并精确重新暂存；git check-ignore对交付路径返回1，git diff --cached --check与git diff --exit-code均返回0。报告追加曾产生EOF多余空行，git diff --check返回2，清理后复核0。原有暂存保留，无未解决检查失败；.superpowers报告与test-artifacts过程输出仍按既有规则排除，未强制入库。等待主控独立复核。

## Task02 round2：可空序号省略语义整改

整改基线staged tree：`cd228443f17c925f08910a342b81b4bc215cba10`。本次只修独立复核的新Important，不重复原四项整改或接通后端。

生产改动仅为IngressEnvelopeV1.per_instance_sequence增加`serde(default, with = "crate::contracts::safe_optional_u64")`。省略字段与显式null均解析为None并返回order_unknown，MAX_SAFE_INTEGER保留原值且无order_unknown，overflow仍拒绝。生成Schema新增default=null，原有可省略required列表、integer/null类型及maximum保持一致；TS无需内容变更。

新增定点回归`optional_sequence_missing_null_and_integer_bounds_match_schema`同时覆盖上述四种输入、显式validate、诊断和Schema结构一致性。

| 实际命令（项目根目录，当前进程工具链环境） | 返回码 | 结果 |
| --- | --- | --- |
| `pwsh -NoProfile -File scripts/dev.ps1 test -p agentnotify-core --test contracts --locked optional_sequence_missing_null_and_integer_bounds_match_schema`（RED） | 101 | 省略字段出现missing field per_instance_sequence，1 failed |
| `pwsh -NoProfile -File scripts/dev.ps1 fmt -p agentnotify-core` | 0 | 只格式化core |
| `pwsh -NoProfile -File scripts/dev.ps1 run -p agentnotify-core --example export_contracts --locked` | 0 | Schema自动生成，TS内容未变 |
| 上述同一定点test命令（GREEN） | 0 | 1 passed，19 filtered out |
| `pwsh -NoProfile -File scripts/dev.ps1 run -p agentnotify-core --example export_contracts --locked -- --check` | 0 | 生成文件无漂移 |
| `pwsh -NoProfile -File scripts/dev.ps1 fmt -p agentnotify-core -- --check` | 0 | 无输出 |

未运行无关CLI、M0或全部workspace。当前无检查阻塞，交回主控独立复核，不自授验收通过。精确交付路径为crates/core/src/event.rs、crates/core/tests/contracts.rs、schemas/event-v1.schema.json和docs/acceptance/m1-contracts.md；本报告仍受.superpowers既有忽略规则排除，临时原始输出在test-artifacts/m1/round2-*。
RED实际完整输出：

```text
   Compiling agentnotify-core v0.1.0 (<workspace>\crates\core)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.35s
     Running tests\contracts.rs (target\debug\deps\contracts-45e2006fbab60508.exe)

running 1 test
test optional_sequence_missing_null_and_integer_bounds_match_schema ... FAILED

failures:

---- optional_sequence_missing_null_and_integer_bounds_match_schema stdout ----

thread 'optional_sequence_missing_null_and_integer_bounds_match_schema' (40740) panicked at crates\core\tests\contracts.rs:517:66:
called `Result::unwrap()` on an `Err` value: Error("missing field `per_instance_sequence`", line: 0, column: 0)
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    optional_sequence_missing_null_and_integer_bounds_match_schema

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 19 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p agentnotify-core --test contracts`
```

GREEN实际完整输出：

```text
   Compiling agentnotify-core v0.1.0 (<workspace>\crates\core)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.14s
     Running tests\contracts.rs (target\debug\deps\contracts-45e2006fbab60508.exe)

running 1 test
test optional_sequence_missing_null_and_integer_bounds_match_schema ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 19 filtered out; finished in 0.00s
```

生成check实际输出：

```text
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.19s
     Running `target\debug\examples\export_contracts.exe --check`
```
