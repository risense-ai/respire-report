<script setup lang="ts">
import { onMounted, ref, watch } from 'vue'
import { useUnsavedChanges } from '../composables/useUnsavedChanges'
import { bridge, errorText, type DiagnosticsReport, type AgentBundleResult, type AgentScenario } from '../services/bridge'
const report = ref<DiagnosticsReport | null>(null)
const update = ref<{ available: boolean; code: string } | null>(null)
onMounted(async () => { try { update.value = await bridge.updateAvailability() } catch { /* 状态未知保持未开放 */ } })
const error = ref(''); const loading = ref(false)
const scenario = ref<AgentScenario>('audit')
const hideLocalPaths = ref(false)
const bundle = ref<AgentBundleResult | null>(null)
const bundleBusy = ref(false)
const bundleMessage = ref('')
const uninstallMode = ref<'keep_settings' | 'remove_settings'>('keep_settings')
const uninstallPlan = ref<Awaited<ReturnType<typeof bridge.previewUninstall>> | null>(null)
const uninstallDialog = ref<HTMLDialogElement | null>(null)
const uninstallBusy = ref(false)
const uninstallError = ref('')
const uninstallReport = ref('')
useUnsavedChanges({ dirty: () => false, busy: () => uninstallBusy.value || bundleBusy.value, discard: () => {} })
watch(uninstallMode, () => { uninstallPlan.value = null; uninstallError.value = '' })
function cancelUninstall() {
  if (uninstallBusy.value) return
  uninstallDialog.value?.close(); uninstallPlan.value = null; uninstallError.value = ''
}
async function previewUninstall() {
  if (uninstallBusy.value) return
  uninstallBusy.value = true; uninstallError.value = ''; uninstallPlan.value = null
  try { uninstallPlan.value = await bridge.previewUninstall(uninstallMode.value); uninstallDialog.value?.showModal() }
  catch (e) { uninstallError.value = errorText(e) }
  finally { uninstallBusy.value = false }
}
async function confirmUninstall() {
  if (uninstallBusy.value || !uninstallPlan.value || uninstallPlan.value.blockers.length) return
  uninstallBusy.value = true; uninstallError.value = ''
  try {
    const launched = await bridge.applyUninstall(uninstallPlan.value.plan_id)
    if (launched.status !== 'helper_started' || !launched.report_path) throw new Error('uninstall_launch_unconfirmed')
    uninstallReport.value = launched.report_path
    uninstallDialog.value?.close(); uninstallPlan.value = null
  } catch (e) { uninstallError.value = errorText(e) }
  finally { uninstallBusy.value = false }
}

watch([scenario, hideLocalPaths], () => { bundle.value = null; bundleMessage.value = '选项已变化，请重新生成协助包后再复制。' })
async function createBundle() {
  bundleBusy.value = true; bundleMessage.value = ''; bundle.value = null
  const selectedScenario = scenario.value; const selectedPrivacy = hideLocalPaths.value
  try {
    const created = await bridge.createAgentBundle(selectedScenario, selectedPrivacy)
    if (scenario.value === selectedScenario && hideLocalPaths.value === selectedPrivacy) bundle.value = created
    else bundleMessage.value = '选项已变化，请重新生成协助包后再复制。'
  }
  catch (e) { bundleMessage.value = errorText(e) }
  finally { bundleBusy.value = false }
}
async function copyPrompt() {
  if (!bundle.value) return
  try { await navigator.clipboard.writeText(bundle.value.prompt); bundleMessage.value = '提示词已复制。发送前请核对下方路径预览；需要分享资料时手动附上脱敏包。' }
  catch { bundleMessage.value = '剪贴板不可用。可从下方文本框手动复制提示词。' }
}
async function load() { loading.value = true; error.value = ''; try { report.value = await bridge.diagnostics() } catch (e) { error.value = errorText(e) } finally { loading.value = false } }
onMounted(load)
</script>
<template><div><p class="page-intro">仅显示本机脱敏状态与错误代码，不包含邮箱授权码、通知正文或客户端私有数据。</p><div v-if="error" class="callout error" role="alert">{{ error }}</div><section class="panel"><div style="display:flex;justify-content:space-between;align-items:center"><h2>当前问题</h2><button class="button" :disabled="loading" @click="load">重新检查</button></div><div v-if="!report?.issues.length" class="empty">暂无后台报告的问题。投递结果仍请以通知记录和收件箱为准。</div><div v-else class="list"><div v-for="issue in report.issues" :key="issue.code" class="list-row"><div><strong>{{ issue.message }}</strong><small class="mono">{{ issue.code }}</small></div></div></div></section><section class="panel"><h2>诊断代码</h2><p class="muted small">仅统计本机诊断代码，不导出事件内容。</p><div v-if="!report?.recent.length" class="empty">没有可显示的诊断代码。</div><div v-else class="list"><div v-for="row in report.recent" :key="row.code" class="list-row"><span class="mono">{{ row.code }}</span><span>{{ row.count }} 次</span></div></div></section><section class="panel"><h2>版本</h2><p>{{ update?.available ? '当前仅支持验证签名后的手动更新，操作说明见包内 START_HERE.md。' : '未签名开发构建；正式更新未开放，缺少发布者信任根。' }}</p><p class="muted small">升级中断可生成“升级恢复”协助包。</p></section>
<section class="panel" aria-labelledby="uninstall-heading">
  <h2 id="uninstall-heading">卸载 AgentNotify</h2>
  <p class="muted small">停止本工具后台并清理可确认归属的安装与接入。不会卸载 Codex 或其他客户端。先预览实际范围，再最终确认。</p>
  <fieldset class="settings-fields" :disabled="uninstallBusy || !!uninstallReport">
    <label class="check-row"><input v-model="uninstallMode" type="radio" value="keep_settings" name="uninstall-mode" />保留邮箱、通知偏好与授权凭据（默认）</label>
    <label class="check-row"><input v-model="uninstallMode" type="radio" value="remove_settings" name="uninstall-mode" />同时清除本工具配置和授权凭据</label>
    <p class="hint">保留设置不保留通知历史、待发队列及旧接入状态；重新安装后重新接入，不补发旧通知。无法确认归属的文件会保留，客户端配置有冲突时先停止卸载。</p>
    <button class="button danger" :disabled="uninstallBusy || !!uninstallReport" @click="previewUninstall">{{ uninstallBusy ? '正在处理…' : '预览卸载范围' }}</button>
  </fieldset>
  <p v-if="uninstallError && !uninstallDialog?.open" class="callout error" role="alert">{{ uninstallError }}</p>
  <div v-if="uninstallReport" class="callout warn" role="status"><strong>清理程序已启动，设置窗口将退出。</strong><p>这不代表卸载完成。处理结果请查看报告：</p><p class="mono">{{ uninstallReport }}</p></div>
</section>
<dialog ref="uninstallDialog" class="unsaved-dialog uninstall-dialog" aria-labelledby="uninstall-confirm-title" @cancel.prevent="cancelUninstall">
  <h2 id="uninstall-confirm-title">确认卸载 AgentNotify</h2>
  <template v-if="uninstallPlan">
    <p>{{ uninstallPlan.mode === 'keep_settings' ? '保留邮箱、通知偏好与授权凭据。' : '将清除本工具配置及归属明确的授权凭据。此操作不能在界面内撤销。' }}</p>
    <h3>将清理</h3><ul><li v-for="(item, index) in uninstallPlan.remove_items" :key="index">{{ item }}</li></ul>
    <h3>将保留</h3><ul><li v-for="(item, index) in uninstallPlan.preserve_items" :key="index">{{ item }}</li></ul>
    <p class="small">涉及本工具凭据 {{ uninstallPlan.credential_count }} 项；{{ uninstallPlan.mode === 'keep_settings' ? '保留范围以上方清单为准。' : '仅清除本工具可确认归属的凭据。' }}</p>
    <div v-if="uninstallPlan.blockers.length" class="callout error" role="alert"><strong>暂不能卸载，请先处理以下问题</strong><ul><li v-for="(item, index) in uninstallPlan.blockers" :key="index">{{ item }}</li></ul></div>
    <p v-if="uninstallError" class="callout error" role="alert">{{ uninstallError }}</p>
    <div class="actions"><button class="button" :disabled="uninstallBusy" autofocus @click="cancelUninstall">保持当前使用</button><button class="button danger" :disabled="uninstallBusy || uninstallPlan.blockers.length > 0" @click="confirmUninstall">{{ uninstallBusy ? '正在启动清理…' : '确定卸载' }}</button></div>
  </template>
</dialog><details class="panel"><summary>可选：Agent 协助与迁移</summary><p class="muted small">日常首次设置请按“邮箱 → 工具接入 → 测试”完成，无需生成协助包。遇到问题或需要迁移偏好时，可生成脱敏资料和完整提示词。Agent 会根据你的实际请求处理；资料本身不会授权修改或发送邮件。</p><div class="form-row"><label for="agent-scenario">求助场景</label><select id="agent-scenario" v-model="scenario"><option value="install">安装接入</option><option value="repair">诊断修复</option><option value="migrate_machine">迁移到新电脑</option><option value="audit">只读核查</option><option value="upgrade_restore">升级恢复</option></select></div><label class="check-row"><input v-model="hideLocalPaths" type="checkbox">复制时隐藏本机绝对路径（需手动附包）</label><button class="button primary" :disabled="bundleBusy || !report?.agent_bundle_available" @click="createBundle">{{ bundleBusy ? '生成中…' : '生成脱敏包与提示词' }}</button><div v-if="bundleMessage" class="callout" role="status">{{ bundleMessage }}</div><div v-if="bundle"><p class="small">包 ID：<span class="mono">{{ bundle.bundle_id }}</span> · 有效至 {{ bundle.expires_at }} · 预演状态：{{ bundle.plan_status }}</p><p class="small">本机包位置：<span class="mono">{{ bundle.bundle_path }}</span></p><div v-if="bundle.path_preview.length" class="callout warn"><strong>复制前路径预览</strong><p class="small">下列绝对路径将进入提示词，可能包含用户名。上传包使用别名，不含原路径。</p><ul><li v-for="path in bundle.path_preview" :key="path" class="mono">{{ path }}</li></ul></div><button class="button primary" @click="copyPrompt">复制给我的 Agent</button><label for="agent-prompt" class="small">可复制提示词</label><textarea id="agent-prompt" :value="bundle.prompt" readonly rows="8" style="width:100%;margin-top:8px" /></div></details></div></template>

<style scoped>
.uninstall-dialog { max-height: calc(100vh - 48px); overflow-y: auto; overflow-wrap: anywhere; }
</style>
