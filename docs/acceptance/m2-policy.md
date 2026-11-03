# M2-B 中立策略与持久看门狗执行证据

日期：2026-10-30。分支：feature/agentnotify-windows；HEAD：231745bd3d2ca22673e2c2d9000727a527f37eef。起始暂存树：3d053aeee09e7e694dd52cb3886140d467682772。本文记录实现者执行结果，独立核验/验收由主代理另行组织。

## 范围与规则版本

本版为 core-neutral-policy-v1，输入契约仍为 IngressEnvelopeV1/schema_version=1。SourceEvidence 唯一来自 AdapterOutput；核心不解析原始厂商协议、Respire JSON、客户端私有库或路径。厂商协议分类规则版本和真实Hook兼容性属于任务07，本证据不声称覆盖。

真实时区库锁定 chrono-tz 0.10.4，随包 IANA_TZDB_VERSION=2025b（本地包 src/prebuilt/directory.rs 实核）；实现使用 IANA 本地时间映射、重叠最早开始/最晚结束、缺口下一个有效分钟。[chrono-tz 官方文档](https://docs.rs/chrono-tz/0.10.4/chrono_tz/)说明其时区实现来自 IANA 数据。

## 已实现链路

- 接收：队列同一 IMMEDIATE 事务写 ingress/lifecycle、Policy参与者、notification、checkpoint；去重、generation、活动及摘要决定也在该事务中提交。
- 公共入口：实现任务02 contracts::Policy trait，返回唯一 PolicyDecision；状态ful路径由 TransactionParticipant 调用，同一规则执行来源/用户/静默判断。未知来源保留，只有VerifiedAdapter+InternalBackground+Success且用户规则允许时过滤正常成功输出；错误不被此规则静默。
- 主体：ActorKey贯穿generation、取消与聚合。Unknown仅稳定event_id可去重；已知actor无turn/可靠Started时，仅在隐私处理后保留完整非空问题/回答证据才做10秒精确短窗；缺证据保留通知。非Ordered不推断generation、不跨事件聚合/取消。generation递增检查MAX_SAFE_INTEGER，数据库亦有范围约束。
- 正常结束：聚合开启时max(最后结束+3秒,首条+10秒)，关闭仍3秒；兄弟actor、实例、会话和错误不混合。合并项保留原始事实，清除待发正文。
- 静默：接收及发送均复查。跨午夜、全天静默、DST重叠/缺口均使用真实IANA规则。skip为终态，修改配置不复活。digest保存计数、最多8条必要标签及关联ID，不复制正文；实际UTC区间/分钟与实例构成唯一桶，摘要不得延长成员原TTL。
- 配额：全局滚动60秒最多10次领取，重试同样消耗配额；超额原项转摘要，下一分钟显式flush_digests写入同一队列。摘要发送前复核取消、revision及TTL，受相同静默和配额控制。
- 看门狗：默认关闭、1800秒、15秒持久tick。可靠Started/actor/轮次能力缺一不可；inactivity还需可靠Progress。可信内部后台不建activity，生成stalled继承原SourceEvidence，以继续遵守用户子代理规则。每轮稳定ID、最多一次告警，不附正文，不称卡死或业务失败。
- 恢复：分别持久OS boot_id/resume_epoch、单调/UTC观察值及独立runtime_id。后台重启、休眠代次、时钟跳变、unknown boot或未知顺序挂起。spool使用明确apply_replayed上下文保守挂起，不以可后跳墙钟判断事件新旧。新的实时可靠Progress/Resumed重建起点，保留alerted；仅开始/结束能力保持挂起至新轮。
- 发送：before_lease预留配额并复核；record_send_stage(..., DataStarted)在相同写事务中再次验证租约token、有效期、当前stage、实例积压/维护、活动revision和当前静默，然后写DATA journal。网络仍在事务外；DATA后的进展不撤回既有发送事实。

## 实际测试与结果

命令通过项目scripts/dev.ps1调用现有工具链，仅当前进程设置环境；所有数据在tempfile隔离目录，没有真实邮件、账号、Hook或客户端访问。

| 命令/阶段 | 实际结果 |
|---|---|
| `test -p agentnotify-core --test classification --test dedupe --test policy_time --test watchdog` 初次 | exit1；修正fixture导入后四组均只因缺policy模块失败（RED） |
| 首次实现后的上述四组 | exit0；5+5+8+10项 |
| 增补leased重启与stalled摘要取消 | exit1；两个目标断言实际失败，随后修复 |
| 15秒tick定点RED | exit1；60秒误出1项而应0，随后持久tick修复 |
| 历史Progress重放RED | exit1；旧Progress误恢复并出1项，随后修复 |
| 墙钟后跳重放、子代理用户规则RED | exit1；两项实际失败，随后明确重放上下文并持久SourceEvidence |
| 共享Policy trait调用RED | exit1/E0277；实现缺trait，随后真实入站链复用该trait |
| `test -p agentnotify-core --locked`，RUST_TEST_THREADS=1 | exit0；classification6、contracts20、dedupe5、policy_time11、queue_recovery33、spool_safety10、watchdog19，共104项；unit/doc各0，无编译警告 |
| 最后两个长事务闭包改为具名方法后：`test -p agentnotify-core --test watchdog --test policy_time --locked` | exit0；19+11项 |
| `run -p agentnotify-core --example export_contracts --locked`；随后生成器`--check` | 均exit0；Schema/TS与唯一Rust配置一致 |
| 限定本任务Rust文件`rustfmt --edition 2024 --check`；`git diff --check` | exit0；Git仅提示已有LF规范化，不存在whitespace错误 |

覆盖：未知保留/可信成功/普通JSON子代理；重复Started、短窗、未知actor稳定ID、父子/兄弟隔离；接收/发送静默、全天、DST、系统时区变化、3/10秒、重试限频、摘要幂等与TTL；默认关闭/能力门禁、进展计时、重启/睡眠/时钟跳变、同会话关闭范围、真实Stop/timeout并发、SQLite锁后spool Stop优先、单轮告警、租约至DATA间进展、DATA后不撤回、历史重放及墙钟后跳。

## 字段同步与后续调用约定

- ConfigV1新增带serde(default)的NotificationRules（聚合、内部成功静默、子代理、等待输入）；旧字段与原选择保留。固定IANA配置通过校验；schemas/config-v1.schema.json和apps/desktop/src/generated/contracts.ts已同步。event-v1 schema无变化。GUI差异展示属于任务08，当前没有GUI迁移验收。
- migration002将库版本升至2；actor_state/activity_state/policy_notifications/dedupe/digests/rate/ticks属于本机内部存储，无外部DTO/Excel/页面字段复制。SourceEvidence按共享Rust类型序列化；生成告警用同一NormalizedEventV1；没有第二套厂商DTO。
- 后台复用SqliteQueue及Policy对象；每15秒刷新ClockSnapshot后evaluate_due，并周期import_spool新快照及flush_digests。ImportReport.remaining=0只是快照提示。suspend_persisted提供显式恢复接点；它受同实例积压门禁，apply_replayed补充覆盖恢复时仍在spool的旧活动。
- 任务05使用真实lease/token接口；发送前调用record_send_stage进入DataStarted再开始网络DATA。构建邮件必须先查询delivery_summary：Some使用计数/标签摘要模板，不能把内部摘要占位信封的ResponseFinished当成单个任务结束。正常聚合计数保存在policy_notifications.aggregate_count。
- 任务06必须提供可靠IANA系统时区、OS启动与恢复证据；独立Hook进程与daemon的运行归属上下文尚需接通。Policy::new创建独立后台运行代次，不能每个Hook随意new后声称跨进程连续性成立。

## 验证边界

真实验证为磁盘SQLite/WAL/事务、独立连接写锁、租约token、线程竞争、持久重开与spool重放。时钟、boot/resume和系统时区均为明确fixture注入，不是Windows实机睡眠/时区切换验收。DirectorySecurity仅验证测试隔离路径；安全目录/OS时钟绑定、真实SMTP/native key、Hook、GUI、macOS/Linux平台、部署与业务验收均未执行。未重跑M0或全workspace。

未决：由任务05/06接通上述发送/调度/系统上下文；任务07验证厂商协议；独立核验/验收尚待主代理组织。没有运行真实邮件或凭据操作。

## 初版输入与测试定义SHA256（fix1更新见下）

以下为本次实际文件字节哈希；测试动态变体由对应Rust测试定义，可复现，无原始厂商协议。
| 文件 | SHA256 |
|---|---|
| tests/fixtures/events/neutral-finish.json | 56549d6e19739c782d08b0c728e4d8f01ab27df768148c16df6be547744079ce |
| crates/core/tests/classification.rs | 39ad318833cac15e014004b5df594d855f2f12331bb9099fb8c965517a3c9f4f |
| crates/core/tests/dedupe.rs | 97937fb4b335030bccb291393ac0b49e6e1915a8b8ee29782d8cbc53315cf1a9 |
| crates/core/tests/policy_time.rs | 5601503e7587b61d2b65f31a6a5ba900001482fc53bc9688a2ea44760ed41577 |
| crates/core/tests/watchdog.rs | 2c882886a5dd165a98d5353e33709a6ed31e2b4d4093c8b3296e1efbb1b4dec6 |

补充运行归属限制：现有Policy::new只生成独立runtime，当前未提供供Hook注入共享受信daemon runtime的构造接口。任务06需按受信平台/IPC边界扩展并验证，不能让每个Hook独立new后声称活动连续。原始事件不得自行声明runtime/能力；suspend_persisted为当前已有的显式恢复入口。

## Fix1：独立验收三项Important整改

本轮基线为e9b437914d1cbe0a4c967bbd0f947f0ce78ea59f；独立验收曾判FAIL，以下为整改执行证据，尚待独立复验。

1. 默认隐私先把问题/回答清成None，原短窗哈希错误把缺证据当相等。真实Ingress::accept传入同actor、无event_id/turn/generation的不同正文，关聚合以隔离去重，第二条实际Suppressed（RED）。修复后短窗只接受隐私处理后完整且非空的问题/回答；None/空/仅部分内容均保守保留，不读取或采集隐私清理前正文。用正常结束及错误两种事件验证Queued，同时断言notifications.body为空、入站元数据无问题/回答。原10秒短窗正向测试改为显式启用两段正文、测试AEAD及相同内容，9秒去重、11秒保留仍成立。
2. DigestPending不是终态，原入队路径及普通transition仍保留body。启用正文、测试安全内容能力及既有FixtureAead，真实入队静默digest和Queued->DigestPending两项均断言失败（RED）；发送时quiet转换原本已清理，作为守护测试保留。新增共享discards_body规则区分“终态”与“需要丢正文的状态”，入队及所有队列状态转换统一使用；三条路径均验证body IS NULL。未宣称SQLite旧页安全擦除。
3. Started(t1)->Started(t2)->重复Started(t1)在upstream_id=None时原来倒回generation=3/current_turn=t:t1（RED）。现于已有policy_dedupe表保存lifecycle_start命名空间的actor+可靠turn/上游开始ID；与通知哈希键不相交，不按短窗过期，检查/记录与generation和活动同事务回滚。历史重入先返回，不关闭当前活动、不改变revision/alerted、不取消当前待发stalled。测试以新Queue、新Policy重开同一磁盘库，确认不是内存幂等；watchdog关闭时亦保留该规则。

实际命令均通过scripts/dev.ps1、现有工具链，仅进程环境设置：

| 命令 | 实际输出/结果 |
|---|---|
| `test -p agentnotify-core --test dedupe --test policy_time --test watchdog fix1_ --locked`，实现前 | exit1；dedupe先失败：left Suppressed/right Queued，cargo默认停止后续target |
| `test -p agentnotify-core --test policy_time --test watchdog fix1_ --no-fail-fast --locked`，实现前 | exit1；time 1 passed/2 failed（初始与transition正文）；watchdog 0 passed/2 failed（3,t:t1 vs 2,t:t2） |
| 同三组fix1_、加`--no-fail-fast`，实现后 | exit0；1+3+2，六条定点全过 |
| `test -p agentnotify-core --test dedupe --test policy_time --test watchdog --test queue_recovery --test spool_safety --locked`，RUST_TEST_THREADS=1 | exit0；dedupe6、policy_time14、watchdog21、queue_recovery33、spool_safety10，共84项全过，无编译警告 |

无公共类型/schema变更，未重新生成契约；未运行M0/全workspace/真实SMTP/系统Hook。runtime跨进程集成仍归任务06，未扩大实现。本轮仅4个生产文件、4个测试/fixture文件及本文，完整过程追加task-4-report.md。

Fix1测试定义SHA256：
| 文件 | SHA256 |
|---|---|
| crates/core/tests/dedupe.rs | f833763b49a5d6987d663f08d10245cf15608294386409c912dccfeefa4640e7 |
| crates/core/tests/policy_time.rs | f3cb06469fadf49b0ff95a33c27bdeee56b1e168feda942e59ce39e730c29c3d |
| crates/core/tests/watchdog.rs | 171c56dc1a07a089b64562d20f18fd78cf27fe67e6abafe14301655c929e69c8 |
| crates/core/tests/support_policy/mod.rs | 7147c61b1137ec0a164c6695524db065a729a39397e6e73bb1e1a7086ff9b8ed |
