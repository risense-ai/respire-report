# M2-A 持久队列实现与验证证据

日期：2026-10-30。范围：任务03；实现者证据，等待独立核验/验收。任务04策略/看门狗、任务05生产AEAD/原生安全密钥和任务12平台性能验收均未在本文冒充完成。

## 已实现边界

- 复用 `core::{event,config,contracts,clock,privacy}`，实现原有 `Ingress` / `Queue`，没有平行的公共事件或错误定义。Core不依赖adapters/platform。
- admission使用OS文件锁，争用最多等待700ms，持锁覆盖独立序号的临时文件写入、fsync、原子替换以及整个envelope的DB或spool提交。Windows原子替换调用 `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`；未修改全局工具链配置。
- SQLite使用真实文件、WAL、FULL同步、外键、1秒busy timeout、入站ID主键与来源事件唯一键。同事务提交不可变生命周期、事务参与者的活动变更、通知和checkpoint。
- 每实例独立序号文件；无序号使用UUID应急spool。`OrderingEvidence::{Ordered,OrderUnknown,SequenceGap,Late}`明确传到事务参与者；降级持久化且不会由后来补序号自动解除。重复入站不重放生命周期；重复来源事件消费已分配序号，避免假缺口。
- 同实例存在spool积压时后续事件继续spool；领取和到期评估同样受积压门禁约束。导入按实例/序号排序，未知项只按UUID作稳定排列；不将这个排列称为真实发生顺序。扫描/AEAD预检在admission锁外，逐项短锁，导入事务busy timeout为5ms，按100ms协作式批次预算返回剩余数量。
- spool固定44字节头：magic4/version2/flags2/metadata_len4/ciphertext_len4/nonce12/tag16。随后为最多8KiB的无正文metadata JSON和原始AEAD密文。正文在AEAD明文中用两个u32长度前缀编码，不把密文base64嵌入JSON。最终单项最多64KiB，总量最多16MiB；隔离文件和崩溃临时文件也占配额。
- `BodyProtector`只有中立接口，没有生产默认密钥或明文后备。两个正文开关先经privacy清理/UTF-8截断；需要正文但没有保护器时不确认接受。SQLite的入站metadata和生命周期均没有正文，正文只在独立加密blob内。
- 十种状态的显式允许矩阵、60秒租约、租约token隔离、单sender OS锁、持久发送阶段、明确未发送失败的30/120/600/1800秒重试及0–20%抖动，最多初次加4次。DATA后结果不确定进入`delivery_unknown`；已持久化accepted恢复为sent。通用transition同样不能绕过DATA边界。
- TTL取配置与24小时的较短值；入站、领取、状态转换和租约恢复执行相关过期处理。认证修复不能跨TTL重新发送。终态将正文列置NULL；不宣称SQLite/WAL安全擦盘。队列是被调用的存储组件，生产后台定时调度仍由后续任务接通。
- maintenance通过带随机操作ID的原子持久标记控制。完整升级/回滚、快照和迁移审计流程属于后续升级任务。

## 实际验证

运行前使用本会话已安装工具链，设置仅当前进程的CARGO_HOME/RUSTUP_HOME/PATH；MSVC由现有 `scripts/dev.ps1` 探测。未把开发者工具链路径写入源码/正式证据。

| 命令/阶段 | 实际结果 |
|---|---|
| `./scripts/dev.ps1 test -p agentnotify-core --test queue_recovery`，首次RED | exit 1，缺少core::queue及尚未接入的存储/测试AEAD依赖 |
| `./scripts/dev.ps1 test -p agentnotify-core --test spool_safety`，首次RED | exit 1，同上；不是平台安全验收结果 |
| 新增接口后的行为RED：`explicit_transition_cannot_retry_after_data_started` | exit 1，实际RetryWait、应为DeliveryUnknown；已修复通用入口 |
| 行为RED：`ttl_is_never_extended_beyond_twenty_four_hours` | exit 1，48小时配置令24小时后仍可领取；已加存储层24小时上限 |
| 行为RED：`transition_to_leased_has_recoverable_expiry` | exit 1，恢复数量0而应为1；已补原子租约期限/token |
| `./scripts/dev.ps1 test -p agentnotify-core --locked`，环境`RUST_TEST_THREADS=1` | exit 0；contracts 20/20、queue_recovery 28/28、spool_safety 10/10；unit/doc-test各0；无编译警告 |

`queue_recovery`的28项中包含一个子进程测试入口；父测试实际启动4个独立进程，验证OS admission与稳定ID唯一入站。不是仅用进程内Mutex模拟并发。

真实SQLite文件/锁证据：WAL pragma、重开队列后记录可见、跨连接BEGIN IMMEDIATE真实写锁约1秒后落spool、同实例Started/Stop重放排序、并发入站和并发重导、全部100种持久状态转换、租约阶段恢复、认证修复/TTL、重试次数/抖动、过期worker token隔离、sender文件锁。真实写锁测试包含单次900ms下界和2秒上界断言；这只是本隔离测试的观察，不是Windows性能验收。

故障注入证据：序号写入前/后、fsync后、rename前/后；spool临时写入、fsync、rename各断点；生命周期前/后、通知后、checkpoint前、提交前、导入提交前/后。断点验证没有部分事务、没有虚假成功、提交后未删spool的重导不双应用生命周期。`disk_full_never_acknowledged`使用存储边界错误注入，没有真的填满用户磁盘，也不是断电测试。

安全证据：本机Windows junction实际创建后被拒绝；生产默认DirectorySecurity拒绝任意目录；测试目录能力仅限fixture自身隔离临时路径。测试AEAD为隔离fixture的AES-256-GCM固定诱饵key，覆盖最大正文、NUL/UTF-8边界、元数据AAD、密文篡改、终态清正文和配额错误。没有读取系统生产密钥、真实邮箱或业务正文。

并行全套测试曾在磁盘争用下出现一次受控AdmissionTimeout。并发测试按契约核对：至少一个持久接受；其余只允许有界AdmissionTimeout，随后按相同稳定事件ID重试，最终仍仅一行。没有延长生产等待预算。最终全套使用串行测试调度，测试内部的线程/独立进程并发保持真实。

## 任务04/05接点

| 接口 | 调用约定 |
|---|---|
| `SqliteQueue::open(root, config, QueueDependencies)` | 显式注入时钟、安全目录验证器、可选BodyProtector、事务参与者和故障器；生产用NoFaults |
| `TransactionParticipant::apply(tx, envelope, order)` | 在已有写事务内应用生命周期/活动/策略并返回NotificationPlan；错误回滚整个envelope。禁止网络/文件IO及另开连接 |
| `NotificationPlan { status, not_before }` | 初始状态只允许queued/suppressed/digest_pending/retry_wait。测试fixture统一queued不代表策略实装 |
| `with_instance_transaction(instance_id, operation)` | 到期评估/看门狗事务入口；maintenance或该实例存在spool时返回None；否则闭包与其写入同提交 |
| `lease_due(now, limit)` | 最多返回100条带私有token的Lease；SMTP在方法返回之后运行 |
| `record_send_stage(&lease, expected, next)` | 带token和期限CAS；开始DATA之前必须成功持久化DataStarted；Accepted必须从DataStarted进入 |
| `complete_lease(&lease, next)` / `retry_temporary(&lease, now, jitter_percent)` | 实际发送worker使用这些带token的接口；retry_temporary负责退避/次数边界，不能用普通transition替代重试调度 |
| `transition(id, expected, next)` | 逻辑/管理状态CAS；用户显式重试DeliveryUnknown才允许回Queued。不是发送worker的完成接口 |
| `acquire_sender()` / `recover_leases(now)` | sender guard持有到全部在途阶段落盘；恢复不自动重发DATA后未确认项目 |
| `set_maintenance(bool)` / `import_spool()` | 新事件在维护/积压期间继续spool；导入报告remaining非零时后续轮次继续，不假设一次清空 |

任务04扩展自身活动表/schema时，需要同步队列迁移入口和版本检查；不要创建第二套Queue/Watchdog存储接口。任务04收到非Ordered证据必须保守处理，禁止推断generation、跨事件取消或追溯改已有通知身份。中立fixture仅证明同事务回调和顺序证据传递，不证明任务04业务逻辑已完成。

## 未执行及限制

- 未实现/验收Windows原生ACL/所有者检查器和系统内容密钥；SystemDirectorySecurity默认失败关闭。预检查拒绝链接的测试不等于完整的句柄级路径竞态防护验收。
- 100ms导入批次和700ms admission/1秒DB busy是协作式运行预算；底层磁盘调用、平台安全验证器和外部事务参与者不具硬实时截止。满16MiB积压的扫描、Hook尾延迟、<300ms目标及无条件2秒预算由任务12真实Windows测量，当前不能宣称满足。
- 没有真实磁盘耗尽、断电、系统重启/休眠试验；重开SQLite和独立测试进程不冒充这些证据。
- 没有真实SMTP、真实Hook安装、用户客户端重启、现有通知工具替换、系统凭据访问、macOS/Linux实机或部署验收。
- TTL处理由存储操作触发，后续后台须定期调用领取/恢复/维护流程；本任务没有运行后台进程，也不宣称停止后台后能主动执行磁盘清理。
## 独立验收整改 round 1（2026-10-30）

整改基线staged tree：`3b9e244d0518ca1c836054748d387da19cf2a548`。独立验收反馈三项Important为规格FAIL/质量FAIL；以下是整改实现与测试证据，等待独立复验，不自授验收通过。本节覆盖前文相应旧实现描述。

1. **未知序号必须在后续生命周期之前传播降级。** `BeforeSequenceRename`失败时，旧实现中A无序号而B/C仍连续，按未知项排最后会令B/C先以Ordered执行。现在在确认无序号envelope落DB/spool前，在独立序号目录原子/fsync持久化版本化order-unknown标记；每次事务在调用participant之前读取该标记。标记不由补回的序号自动清除，DB不可写也不影响它。兼容旧应急spool时，未知项排在同实例已知项之前，只为先传播保守降级，不宣称恢复真实发生顺序。回归覆盖A(None)→B(1)→C(2)，顺序导入与三个独立SqliteQueue句柄并发导入；所有participant均收到非Ordered，checkpoint持续降级。
2. **已初始化队列的构造不再写schema。** 仅user_version=0时执行WAL初始化与001迁移；version=1重开只读版本/WAL证据。真实双连接证明另一个BEGIN IMMEDIATE持锁时，即使`PRAGMA user_version=1`与原值相同也返回SQLite `DatabaseBusy`。整改后新队列对象与独立测试进程均可在该锁仍存在时打开已有WAL库，accept经过真实busy等待后确认Spooled；解锁后重复导入仍只一条通知/一次participant。
3. **100ms协作预算覆盖整次导入且保留推进活性。** deadline在进入import_spool时建立，扫描/AEAD、扫描状态互斥、admission等待和SQLite共用它；admission只等剩余时间，得锁及每个后续边界重新检查。没有在全量扫描之后重新计时，也没有再取Hook的700ms预算。目录游标和增量BTreeMap候选跨调用保留；快照扫描未完成前不执行participant，完整后才按保守顺序导入。期限耗尽保留游标/候选/文件并返回待办提示；事务提交前过期回滚，提交后未删文件仍按同一ID幂等恢复。扫描进度是可重新建立的进程内性能状态，持久顺序降级和实际envelope仍在磁盘，不依赖扫描缓存保证正确性。

`ImportReport.remaining`现在明确为当前快照的待办提示：扫描未完成或其他导入者正在推进时保守至少1；零表示本快照完成，不是并发目录的原子总数。后台应保留同一个队列对象跨轮推进并定期启动新快照；接收和发送门禁仍检查真实spool积压。

新增五个回归首先运行`./scripts/dev.ps1 test -p agentnotify-core --test queue_recovery round1_ --locked`，实际exit1、0/5通过，分别复现：后续2条错误Ordered；锁下构造StorageFailed；新子进程失败；admission在锁仍被占用时不能按预算返回；扫描/解密全量结束后才起算预算。随后定点执行`round1_unknown_sequence`、`sqlite_writer_lock`、`round1_import`，分别1/1、2/2、2/2通过。

关联全集命令：当前进程`RUST_TEST_THREADS=1`，`./scripts/dev.ps1 test -p agentnotify-core --test queue_recovery --test spool_safety --locked`。实际exit0：queue_recovery **33/33**，spool_safety **10/10**，无编译警告。SQLite同值user_version断言细化为DatabaseBusy后，`sqlite_writer_lock`两项再次通过。rustfmt限定本轮Rust路径检查通过。

预算/活性测试的证据边界：一项保持真实admission文件锁，要求导入在锁仍被占用时返回未消费积压；另一项使用8个合法AES-GCM spool、单次open人为延时60ms的中立fixture，首轮不执行participant，后续最多30轮完整导入，未知序号之后没有Ordered。后者用于验证计时起点、游标推进及不越过未完成扫描，不是Windows性能实测。无法抢占的单次系统IO/外部同步回调仍可能越过截止时刻；返回后不会再重置预算或启动新的生命周期提交。<300ms/满额积压/硬性2秒预算继续留任务12。

本轮没有执行M0、全workspace、真实SMTP、平台重启或凭据操作。原有平台ACL/系统密钥默认失败关闭和其他限制保持如实记录。