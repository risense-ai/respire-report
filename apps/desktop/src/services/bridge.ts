export type UninstallMode = 'keep_settings' | 'remove_settings'
export interface UninstallPreview { plan_id: string; mode: UninstallMode; remove_items: string[]; preserve_items: string[]; blockers: string[]; credential_count: number }
export interface UninstallLaunch { status: 'helper_started'; report_path: string }
import { invoke } from '@tauri-apps/api/core'
import type { ConfigV1, DeliveryOutcome, HealthReport, IntegrationStatus } from '../generated/contracts'

export interface ConfigSnapshot { config: ConfigV1; revision: string; watchdog_ready: boolean; inactivity_ready: boolean }
export interface MailView { sender: string; recipient: string; provider: string; host: string; port: number; tls: string; username: string; has_secret: boolean }
export interface MailSnapshot { mail: MailView; revision: string }
export interface SaveReport { saved: boolean; active: boolean; reason: string; revision: string }
export interface DiscoveryOverride { adapter_id: 'codex' | 'claude_code' | 'workbuddy'; config_dir: string; executable?: string }
export interface InspectedClient { adapter_id: string; config_path: string | null; status: IntegrationStatus; reason: string; candidate_id: string; instance_id: string | null; executable: string | null; version: string | null; supported: boolean; completion_channel: string; configured: boolean; event_count: number; last_event_at: number | null; smtp_accepted_count: number; live_verified: boolean }
export interface InspectionReport { schema_version: number; clients: InspectedClient[]; checked_at: string }
export interface PlanInput { adapter_id: string; product_version: string; config_path: string; client_executable: string; instance_id?: string | null }
export interface PlanSummary { operation_id: string; original_hash: string | null; planned_hash: string; events: string[]; status: string }
export interface VerificationReport { operation_id: string; config_matches_plan: boolean; binding_matches: boolean; status: string; live_verified: boolean; completion_channel: string; reason: string }
// 验证游标来自本机登记及发起时间，只回传给当前接入查询。
export interface LiveVerificationCursor {
  instance_id: string
  adapter_id: string
  channel: 'hook' | 'local_observer'
  started_at: number
  last_sequence: number
}
export interface LiveVerificationEvent {
  received_at: number
  status: string
  smtp_accepted: boolean
}
export interface HistoryRow {
  id: string; status: string; received_at: number; kind: string
  project: string; session: string; source: string; occurred_at: string | null
  attempts: number; send_stage: string; policy_reason: string | null; diagnostics: string[]
}
export interface DiagnosticRow { code: string; count: number }
export interface DiagnosticsReport { issues: { code: string; message: string }[]; recent: DiagnosticRow[]; agent_bundle_available: boolean }
export type AgentScenario = 'install' | 'repair' | 'migrate_machine' | 'audit' | 'upgrade_restore'
export interface AgentBundleResult { bundle_id: string; bundle_path: string; path_preview: string[]; prompt: string; expires_at: string; plan_status: string }
export interface TestPreview { confirmation: string; sender: string; recipient: string; subject: string; note: string }
export interface TestReport { outcome: DeliveryOutcome; smtp_accepted: boolean; recipient_confirmed: boolean }
export interface ControlReport { code: string; running: boolean; installed: boolean }

// 仅显式白名单命令；秘密在调用完成后由页面立即清空，不存入浏览器存储。
export const bridge = {
  desktopInfo: () => invoke<{ version: string; build_id: string; executable: string; data_dir: string; runtime_issue: string | null }>('desktop_info'),
  updateAvailability: () => invoke<{ available: boolean; code: string }>('update_availability'),
  health: () => invoke<HealthReport>('get_health'),
  loadConfig: () => invoke<ConfigSnapshot>('load_config'),
  saveConfig: (config: ConfigV1, expectedRevision: string) => invoke<SaveReport>('save_config', { config, expectedRevision }),
  loadMail: () => invoke<MailSnapshot>('load_mail'),
  saveMail: (input: MailView & { secret?: string | null }, expectedRevision: string) => {
    const { has_secret: _ignored, ...safeInput } = input
    return invoke<SaveReport>('save_mail', { input: safeInput, expectedRevision })
  },
  inspectIntegrations: (overrides: DiscoveryOverride[] = []) => invoke<InspectionReport>('inspect_integrations', { overrides }),
  planIntegration: (input: PlanInput) => invoke<PlanSummary>('plan_integration', { input }),
  applyIntegration: (operationId: string) => invoke<VerificationReport>('apply_integration', { operationId }),
  startIntegrationVerification: (instanceId: string) => invoke<LiveVerificationCursor>('start_integration_verification', { instanceId }),
  pollIntegrationVerification: (cursor: LiveVerificationCursor) => invoke<LiveVerificationEvent | null>('poll_integration_verification', { cursor }),
  history: () => invoke<HistoryRow[]>('get_history'),
  diagnostics: () => invoke<DiagnosticsReport>('get_diagnostics'),
  createAgentBundle: (scenario: AgentScenario, hideLocalPaths: boolean, outputDirectory?: string) => invoke<AgentBundleResult>('create_agent_bundle', { scenario, hideLocalPaths, outputDirectory }),
  installationInfo: () => invoke<{ data_dir: string; installed: boolean; can_choose_directory: boolean; first_use_prompt: string }>('installation_info'),
  selectDataDirectory: (directory: string) => invoke<void>('select_data_directory', { directory }),
  previewUninstall: (mode: UninstallMode) => invoke<UninstallPreview>('preview_uninstall', { mode }),
  applyUninstall: (planId: string) => invoke<UninstallLaunch>('apply_uninstall', { planId }),
  confirmClose: () => invoke<void>('confirm_close'),
  desktopReady: () => invoke<void>('desktop_ready'),
  confirmQuit: () => invoke<void>('confirm_quit'),
  previewTest: () => invoke<TestPreview>('preview_test'),
  sendTest: (confirmation: string) => invoke<TestReport>('send_test', { confirmation, explicit: true }),
  control: (action: 'status' | 'install' | 'start' | 'stop' | 'pause' | 'resume') => invoke<ControlReport>('control_background', { action }),
}

export function errorText(error: unknown): string {
  const code = String(error)
  const messages: Record<string, string> = {
    unsafe_owner_or_acl: '当前数据目录的所有者不符合安全要求。请使用已安装的 AgentNotify 启动入口，并核对下方数据位置。',
    unsafe_acl: '当前数据目录的访问权限不符合安全要求。请从已安装入口打开；可在诊断中生成协助信息。',
    ownership_conflict: '安装归属或启动配置与当前实例不一致。请使用已安装入口，避免从旧程序包重复安装。',
    tray_unavailable: '系统托盘未能启动，请保持窗口打开并查看诊断。',
    maintenance_in_progress: '正在升级维护，暂时不能保存设置。请完成升级或恢复后重试。',
    configuration_busy: '设置或维护操作正在进行，请稍后重试。',
    config_conflict: '设置已在别处改变。请重新载入后再保存。',
    invalid_mail_address: '邮箱地址格式不符合当前版本要求，请填写普通 ASCII 邮箱地址。',
    invalid_smtp_config: 'SMTP 设置无效，请检查主机、端口和加密方式。',
    secret_required: '首次设置邮箱需要填写授权码。',
    credential_write_failed: '授权码未能写入系统安全存储，请检查当前用户权限。',
    content_key_unavailable: '系统安全存储不可用，暂时不能开启正文通知。',
    watchdog_capability_missing: '已接入的工具缺少可靠开始或进展事件，无法启用该观察方式。',
    oauth_publisher_registration_missing: '此服务商需要发布者 OAuth 应用，当前版本尚未具备，不能改用普通密码。',
    background_not_installed: '设置已保存；安装并启动本工具后台后生效。',
    background_start_failed: '设置已保存，但后台启动未确认。请到总览检查后台状态。',
    background_restart_failed: '设置已保存，但后台重启未确认。请到总览检查后台状态。',
    needs_install: '请先在总览安装本工具后台，再生成接入预演。Hook必须使用归属已验证的稳定启动器。',
    cli_binary_missing: '无法定位当前 AgentNotify 程序，接入和后台安装暂不可用。',
    cli_timeout_outcome_unknown: '操作等待超时，结果尚未确认。后台操作可能仍在处理，请先重新检测状态，勿立即重复应用或安装。',
    uninstall_plan_conflict: '设置或接入在预览后已改变，请重新预览卸载范围。',
    owned_hook_conflict: '本工具的接入项已被修改，不能自动移除。请先通过 Agent 协助核对。',
    preview_expired: '测试邮件确认已过期，请重新预览。',
    credential_unavailable: '系统安全存储中的授权码暂不可读取，请重新保存邮箱设置。',
    binding_missing: '当前工具尚未登记接入。请先完成接入，再验证真实任务。',
    live_verification_unavailable: '暂时无法读取验证记录。请确认本机后台状态，再重试。',
    live_verification_channel_changed: '完成通知通道在验证期间已变更。请重新检测，再开始验证。',
    live_verification_expired: '本次验证已超时。可重新开始验证。',
    live_verification_unsupported: '当前接入类型暂不支持验证真实任务。',
    unsupported_version: '当前工具版本尚未验证，不能写入接入配置。',
    bundle_create_failed: '无法在本机安全目录创建协助包。',
    bundle_destination_invalid: '协助包输出目录不可用，请重新生成。',
    bundle_parent_unsafe: '协助包目录包含不安全的链接，请检查本机目录。',
    bundle_write_failed: '协助包未写入完整，请重新生成。',
  }
  return messages[code] ?? `操作未完成（${code}）。检查诊断页后重试。`
}

export function saveText(report: SaveReport): string {
  return report.active ? '设置已保存，后台已采用新设置。' : errorText(report.reason)
}
