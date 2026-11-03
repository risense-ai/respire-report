# M3 / 任务05 实现与实际证据

日期：2026-10-30。当前实测系统：Microsoft Windows NT 10.0.26300.0。实现基线 staged tree：`842f13f4b29ad8e79d0ece21ed17178f0f026803`，HEAD未改变。这里是执行证据，不是独立验收或真实收件证明。

用户末段明确要求“减少测试，实现功能后我来测试”。收到后没有新增测试或启动新测试命令，仅让此前正在执行的命令收尾并交付。后续功能由用户验证；主代理另行静态审查。

## 生产实现

- `delivery/smtp.rs`：隐式TLS及强制STARTTLS、证书验证、升级后重新EHLO、PLAIN/XOAUTH2、严格有界完整响应解析。多地址有界尝试；不允许明文认证。服务器正文/凭据不进入错误输出。
- `QueueSendJournal`持有实际`Lease`，每个阶段绑定delivery ID及租约token。`DataStarted`事务成功返回后才写网络DATA；网络不在SQLite写事务内。门槛复用已有lease/积压/quiet/TTL/revision检查。
- 完整4xx/5xx拒收经私有`complete_smtp_rejection`同事务完成租约，保留DataStarted阶段证据。半行、解析失败、断线、超时、Accepted落盘失败均不按明确拒收处理，DATA后不确定默认不重发。4xx退避最多5次总尝试，5xx永久失败，认证拒绝进入auth_blocked。
- `prepare_queued_mail`使用真实`Policy::delivery_summary`，普通聚合读取`aggregate_count`。DATA门槛提交后再次构建摘要，避免检查前过期计数。纯文本MIME、CRLF/地址验证、稳定Message-ID、默认无正文；Message-ID不是收件端去重保证。
- `AeadBodyProtector`用系统随机nonce的AES-256-GCM，认证元数据，沿用固定二进制nonce/tag/spool格式。生产构造需要安全存储取回的32字节密钥，无fixture/default key。
- `NativeCredentialStore`固定Windows原生后端。秘密以最多32个2048字节随机分片存系统凭据库，版本化清单校验长度/SHA256；支持至64KiB秘密。按引用的Global命名mutex覆盖跨进程读写删除，2秒等锁失败关闭。原生pending清单先于新分片写入；完整读回后切root，随后清旧分片。中断恢复仅访问该引用及清单列出的随机条目，不枚举账户。提交后清理错误保留intent并报告，下次操作恢复已发布版本，不伪称整个保存无副作用。
- `create_content_key`只显式创建全新随机引用，`load_content_key`只读。读取/权限失败不覆盖旧密钥。宿主需在配置锁内发布所选引用，并删除未发布的初始化引用；这是任务06配置/进程归属接点。
- `OAuthSession`本地IPv4回环随机端口/路径、短生命单次事务、state、S256 PKCE、重复参数/Host/redirect校验。无ID token和openid scope，因此没有伪造access-token nonce校验。`HttpsTokenEndpoint`实际实现HTTPS code兑换/refresh/revoke，禁止重定向，响应有界，无client secret。token仅存CredentialStore；刷新失败停发，远端撤销不支持/失败仍删除本机token并报告实际结果。
- `providers-v1.json`：QQ/163/Gmail为conditional；Outlook/Microsoft365缺发布者资料为unavailable。正式更新信任根未交付，`install_update`明确Unavailable；独立Ed25519验签/防回滚机制使用隔离测试key验证，未将测试key放入生产信任。
- `maintain_retention`：按到期时间清暂存正文、按配置默认30天批量删除历史入站元数据（每轮最多500）；保留运行态与checkpoint。没有把SQLite/WAL删除称作安全擦除。

## 已实际运行的命令与结果

所有cargo命令经现有`scripts/dev.ps1`导入MSVC，只改变当前进程工具链环境。没有安装全局工具或改变PATH持久设置。

| 阶段/命令 | 实际结果 |
|---|---|
| 首次core `test -p agentnotify-core --test smtp_faults --test mail_privacy --test oauth` | exit1；缺delivery实现。初始fixture误用advance也已按实际time API纠正 |
| 首次platform `test -p agentnotify-platform --test credentials_native` | exit1；缺原生backend/依赖 |
| 首版编译 | exit1；serde函数项生命周期约束，改为显式借用闭包后编译通过 |
| 初版SMTP | 实际localhost首先解析`::1`而fixture只监听IPv4；按精确路径停止挂起测试，生产改为有界多地址尝试，fixture加accept期限 |
| 初版SMTP故障集 | 12/13；Progress已在prepare前取消租约，测试调整为SMTP DataStarted前注入；后通过 |
| retention定点RED | 修正测试括号后exit1：缺maintain_retention；实现后通过 |
| AEAD fixture | 两次open拒绝，定位为fixture原路径/规范路径不一致；显式以规范路径创建后通过，不是加密失败 |
| HTTPS OAuth定点RED | exit1：缺隔离CA注入构造；增加受信TLS配置构造后，真实本地TLS code/refresh/revoke通过 |
| 8KiB原生秘密RED | exit1：Windows单凭据条目写入失败；原生分片后通过 |
| 原生并发替换RED | 真实Missing，停止该测试进程；加入按引用mutex与pending恢复后通过 |
| 原生依赖增补后的`--locked` | exit1：需刷新lock；`--offline`编译成功，后续保持locked |
| 最后原生命令：`target/debug/deps/credentials_native-56a8fa676ad44c63.exe --ignored --nocapture --test-threads=1` | exit0；6实际场景+1独立子进程辅助入口。随机命名空间读写删除、8KiB替换、线程并发、双进程并发、初始化互不覆盖、发布前后pending恢复 |
| 最后已启动的core命令：`./scripts/dev.ps1 test -p agentnotify-core --test smtp_faults --test mail_privacy --test oauth --test queue_recovery --test spool_safety --test policy_time --test watchdog --locked`，`RUST_TEST_THREADS=1` | exit0；7+7+15+33+10+14+21=107，均通过，无编译警告；不是全workspace/M0复测 |

本轮Rust文件精确列表执行rustfmt；不格式化其他任务文件。公共外部契约/config/event未改，Schema/TS不需重生成；新增MailConfig/SmtpConfig/OAuth配置仅为内部宿主接点。

## 证据范围及未验证事项

真实：磁盘SQLite、loopback TLS SMTP、loopback HTTPS token HTTP请求/响应、Windows Credential Manager、线程/独立进程锁与读写。注入：OAuth假授权服务器的业务结果、pending中断状态、策略时钟。没有公网发信，没有真实邮箱登录、token或授权码，没有改旧CodexNotifier或真实客户端Hook，也没有重启客户端/操作系统。

未验证/后续接通：真实邮箱收件/服务商政策、发布者OAuth注册、provider正式签名信任根、Windows10 22H2/其他Windows版本矩阵、macOS/Linux原生后端（当前明确Unavailable）、Linux锁定/无服务实机、系统浏览器/UI完整授权流程。任务06仍需将当前配置/Clock、CredentialRef、内容key可用性、目录ACL验证、单发送器锁、spool导入/过期维护与`deliver_lease`接入唯一后台。此任务未实现跨进程runtime所有权。

缺内容key时现有入站隐私边界失败关闭，不接收请求保留的正文；宿主需切换为仅元数据收集并展示原因，不得把BodyProtectionUnavailable当作已安全收集。过期spool需由宿主定时导入/隔离维护，不将本次DB清理API声称为脱离后台也自动生效的留存承诺。

SMTP/DNS与底层系统IO仍有操作系统时延；阶段门槛保证失效租约不写DATA，不承诺所有阻塞系统调用可被硬抢占。用户显式重试delivery_unknown可能重复，沿用现有显式状态转换，不自动执行。

## 服务商依据

本轮复用主代理提供的2026-10-30官方资料调研，不当作实际账号验收。Gmail条件：[应用密码](https://support.google.com/accounts/answer/185833)、[SMTP参数](https://support.google.com/a/answer/176600)。Outlook现代认证：[Outlook.com SMTP设置](https://support.microsoft.com/en-us/outlook/pop-imap-and-smtp-settings-for-outlook-com)、[SMTP OAuth](https://learn.microsoft.com/en-us/exchange/client-developer/legacy-protocols/how-to-authenticate-an-imap-pop-smtp-application-by-using-oauth)。Google桌面授权：[Native OAuth](https://developers.google.com/identity/protocols/oauth2/native-app)。QQ/163具体参数与账号条件见提供的`AgentNotify-provider-research.md`；本轮未登录官网账户。

## 2026-11-02 静态审查定向修复（未运行测试）

基线：`13489006b24217d6851f5810eb07448411210393`。只修复两项审查问题；遵守用户减少测试要求，没有新增用例、故障注入、原生凭据操作或测试运行。旧107项/原生场景结果属于修复前版本，不作为本次行为验收证据。

1. OAuth撤销/刷新竞态：`contracts.rs:398/432`新增仅内存CredentialRevision及默认失败关闭的`replace_if_revision`，没有get+put模拟CAS。Windows `credentials.rs:103/200`以包含每次随机part IDs的完整manifest摘要作版本，非空秘密即使内容相同也具有新发布版本；历史空秘密不提供版本。`credentials.rs:335`在同引用原生mutex内完成存在性/版本核验，再调用不重复加锁的`put_locked`（230行）完成发布。Missing或版本改变直接false，不写pending、不创建root。网络请求始终不持锁。
2. `oauth.rs:578`读取带版本快照，HTTPS刷新后`save_tokens`（532行）只走条件替换；引用删除/变化返回Revoked，不使用网络结果重新创建。`revoke`（627行）改为先完成本地delete，再调用远端；本地删除失败直接返回SecureStorage且不发远端请求，远端失败不恢复本地授权。CAS先完成则后续delete删除新版本；delete先完成则旧CAS因Missing失败。若删除后同引用被显式重建，随机manifest版本也使旧CAS失败。已借出的内存句柄/已发出的网络请求不能撤回，本修复保证的是不会持久复活已断开的授权。
3. `mail.rs:20/43`保留typed MailError及原QueueError；配置校验先于事件隐私校验。`smtp.rs:475`将邮件准备失败接入统一结果处理：DB/摘要读取等队列错误→Retryable并沿用有界退避/TTL；BodyProtectionUnavailable或内容key缺失→AuthBlocked；配置/Schema不可用→AuthBlocked；只有明确非法地址、标签、头或事件→PermanentFailure。可恢复状态保留未到TTL的加密正文；队列本身无法落库时返回QueueError，不虚构已重试或已保存。退避达到既有次数上限/TTL后的终态规则未改变。

既有`crates/core/tests/oauth.rs`的Store fixture仅同步版本读取及Mutex内CAS，以符合新trait；未新增用例、未执行该fixture或任何测试。CredentialRevision/Handle及MailError不是序列化外部DTO，现有Schema/TS定义没有变化，不重生成。

必要编译：`./scripts/dev.ps1 check -p agentnotify-core -p agentnotify-platform --locked`实际exit0（最后1.49秒，无警告）。只使用当前进程工具链设置；对六条触及Rust文件定点rustfmt。并发行为按上述临界区/控制流静态说明，交主代理独立复核与用户功能验证。
