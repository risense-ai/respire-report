<script setup lang="ts">
import { computed, onMounted, ref, shallowRef, watch } from 'vue'
import { bridge, errorText, type InspectedClient, type PlanInput, type PlanSummary, type VerificationReport } from '../services/bridge'
import { changedFieldLabels, useUnsavedChanges } from '../composables/useUnsavedChanges'
const emit = defineEmits<{ changed: []; navigate: [page: 'mail' | 'overview'] }>()
const clients = ref<InspectedClient[]>([])
const selected = ref<InspectedClient | null>(null)
const form = ref<PlanInput>({ adapter_id: 'codex', product_version: '', config_path: '', client_executable: '' })
const configDirectory = shallowRef('')
const baseline = shallowRef('')
const plan = ref<PlanSummary | null>(null)
const applied = ref<VerificationReport | null>(null)
const error = shallowRef(''); const message = shallowRef(''); const busy = shallowRef(false); const detecting = shallowRef(false)
const operation = shallowRef<'preview' | 'install' | 'apply' | 'assistance' | ''>('')
const checkedAt = shallowRef(''); const advanced = shallowRef(false)
const assistance = shallowRef(''); const assistancePath = shallowRef('')
const draftValue = () => JSON.stringify({ form: form.value, directory: configDirectory.value })
watch(draftValue, () => { plan.value = null; applied.value = null })
const dirty = computed(() => !!baseline.value && draftValue() !== baseline.value)
const changes = computed(() => baseline.value ? changedFieldLabels({ form: form.value, directory: configDirectory.value }, JSON.parse(baseline.value), {
  'form.product_version': '客户端版本', 'form.client_executable': '客户端程序位置',
  'form.config_path': '客户端配置文件', directory: '检测目录',
}) : [])
function discard() { if (baseline.value) { const value = JSON.parse(baseline.value); form.value = value.form; configDirectory.value = value.directory }; plan.value = null }
const { requestAction } = useUnsavedChanges({ dirty: () => dirty.value, changes: () => changes.value, busy: () => busy.value || detecting.value, discard })
const names: Record<string, string> = { codex: 'Codex', claude_code: 'Claude Code', generic: '通用事件', workbuddy: 'WorkBuddy' }
const statusText: Record<string, string> = { not_detected:'未发现', detected:'已发现', unsupported_version:'版本不支持', research_pending:'暂不支持', configured:'已接入', trust_pending:'等待客户端信任', reload_pending:'等待客户端重新加载', probe_passed:'探针通过', live_verified:'已有入站记录', degraded:'需要检查' }
const editable = computed(() => selected.value?.adapter_id === 'codex' || selected.value?.adapter_id === 'claude_code')
function choose(client: InspectedClient) {
  selected.value = client
  form.value = { adapter_id: client.adapter_id, product_version: client.version ?? '', config_path: client.config_path ?? '', client_executable: client.executable ?? '', instance_id: client.instance_id }
  configDirectory.value = ''; baseline.value = draftValue(); plan.value = null; applied.value = null; error.value = ''; message.value = ''
}
async function load(manual = false) {
  if (detecting.value || busy.value) return
  detecting.value = true; error.value = ''; message.value = ''
  try {
    const overrides = manual && editable.value && configDirectory.value.trim()
      ? [{ adapter_id: selected.value!.adapter_id as 'codex' | 'claude_code', config_dir: configDirectory.value.trim(), executable: form.value.client_executable || undefined }] : undefined
    const report = await bridge.inspectIntegrations(overrides)
    clients.value = report.clients; checkedAt.value = report.checked_at
    // 检测只更新候选；已编辑的版本或路径不被后台结果覆盖。
    if (selected.value && !dirty.value) {
      const fresh = clients.value.find(item => item.candidate_id === selected.value?.candidate_id)
      if (fresh) choose(fresh)
    }
    message.value = `检测完成：找到 ${clients.value.filter(item => item.executable).length} 个客户端候选。${dirty.value ? '当前手动输入已保留。' : '请选择要接入的工具。'}`
  } catch (e) { error.value = errorText(e) }
  finally { detecting.value = false }
}
async function preview() {
  operation.value = 'preview'; busy.value = true; error.value = ''; message.value = ''; plan.value = null; applied.value = null
  try { plan.value = await bridge.planIntegration(form.value); message.value = '接入方案已准备，请核对后应用。' }
  catch (e) { error.value = errorText(e) }
  finally { busy.value = false; operation.value = '' }
}
async function installBackground() {
  operation.value = 'install'; busy.value = true; error.value = ''; message.value = ''
  try { const result = await bridge.control('install'); message.value = result.installed ? '后台已安装，可继续准备接入方案。' : '后台安装未确认，请到总览查看状态。'; emit('changed') }
  catch (e) { error.value = errorText(e) }
  finally { busy.value = false; operation.value = '' }
}
async function apply() {
  if (!plan.value) return
  operation.value = 'apply'; busy.value = true; error.value = ''
  try {
    applied.value = await bridge.applyIntegration(plan.value.operation_id)
    if (applied.value.config_matches_plan && applied.value.binding_matches) baseline.value = draftValue()
    emit('changed')
    const refreshed = await bridge.inspectIntegrations(); clients.value = refreshed.clients; checkedAt.value = refreshed.checked_at
    const fresh = clients.value.find(item => item.candidate_id === selected.value?.candidate_id)
    if (fresh) selected.value = fresh
  } catch (e) { error.value = errorText(e) }
  finally { busy.value = false; operation.value = '' }
}
async function createAssistance() {
  operation.value = 'assistance'; busy.value = true; error.value = ''; assistance.value = ''; assistancePath.value = ''
  try { const bundle = await bridge.createAgentBundle('repair', true); assistance.value = bundle.prompt; assistancePath.value = bundle.bundle_path }
  catch (e) { error.value = errorText(e) }
  finally { busy.value = false; operation.value = '' }
}
async function copyAssistance() { try { await navigator.clipboard.writeText(assistance.value) } catch { error.value = '复制失败，请手动选择提示词。' } }
function time(value: string) { const date = new Date(/^\d+$/.test(value) ? Number(value) : value); return Number.isNaN(date.getTime()) ? '时间未记录' : date.toLocaleString('zh-CN') }
onMounted(() => load())
</script>
<template>
  <div>
    <p class="page-intro">选择一个工具，核对检测结果后接入。可逐项接入多个工具；首次使用无需生成迁移协助包。</p>
    <div v-if="error" class="callout error" role="alert">{{ error }}</div>
    <div v-if="message" class="callout" role="status">{{ message }}</div>
    <section class="panel">
      <div class="section-heading"><h2>选择要接入的工具</h2><button class="button" :disabled="busy || detecting" @click="load()">{{ detecting ? '正在检测…' : '重新检测' }}</button></div>
      <p class="muted small" role="status">{{ detecting ? '正在核对本机路径与版本，请稍候。' : checkedAt ? `上次检测：${time(checkedAt)}` : '尚未完成检测。' }}</p>
      <div class="grid">
        <button v-for="client in clients" :key="client.candidate_id" :class="['agent-choice', { selected: selected?.candidate_id === client.candidate_id }]" :aria-pressed="selected?.candidate_id === client.candidate_id" :disabled="busy || detecting" @click="requestAction(() => choose(client))">
          <strong>{{ names[client.adapter_id] ?? client.adapter_id }} <span class="status-chip">{{ statusText[client.status] ?? '状态待确认' }}</span></strong>
          <small>{{ client.version ? `版本 ${client.version}` : '尚未获取版本' }} · {{ client.configured ? '已登记接入' : '未登记接入' }}</small>
          <small>{{ client.config_path || '未发现配置路径' }}</small>
        </button>
      </div>
      <p v-if="!clients.length && !detecting" class="empty">没有可用候选。可重新检测；仍未找到时到诊断页获取协助。</p>
    </section>
    <section v-if="selected" class="panel">
      <div class="section-heading"><h2>{{ names[selected.adapter_id] ?? selected.adapter_id }} 接入</h2><span v-if="dirty" class="dirty-label">手动输入尚未应用</span></div>
      <p>{{ !selected.supported ? '此候选尚不满足自动接入条件，请查看检测说明。' : selected.configured ? '已有接入记录；仍需核对当前状态与实际收件。' : selected.supported ? '已找到受支持的客户端，可准备接入。' : '此候选尚不满足自动接入条件，请查看检测说明。' }}</p>
      <p class="small">{{ selected.completion_channel === 'local_observer' ? '完成通知使用本机只读观察接入。' : selected.completion_channel === 'hook' ? '使用客户端事件接入，可能需要确认信任。' : selected.completion_channel === 'explicit_emit' ? '通用事件需主动调用，不会自动监听客户端。' : '该工具或版本暂不支持自动接入。' }}</p>
      <p class="small">历史入站记录 {{ selected.event_count }} 条（含规则抑制） · SMTP 接受 {{ selected.smtp_accepted_count }} 条。{{ selected.live_verified ? '已有入站证据；不代表当前自然任务或收件已验证。' : '当前接入尚未取得有效入站证据。' }}</p>
      <details class="advanced-panel"><summary>检测说明</summary><p class="small">{{ selected.reason || '未记录其他信息。' }}</p><p class="mono small">{{ selected.executable || '程序位置未找到' }}</p></details>
      <template v-if="editable">
        <details :open="advanced" class="advanced-panel" @toggle="advanced = ($event.target as HTMLDetailsElement).open">
          <summary>高级：手动指定配置目录、程序与版本</summary>
          <fieldset class="settings-fields" :disabled="busy || detecting">
            <div class="field"><label for="config-directory">配置目录（可选，用于检测自定义安装）</label><input id="config-directory" v-model.trim="configDirectory" placeholder="客户端配置目录" /></div>
            <div class="form-grid"><div class="field"><label for="version">客户端精确版本</label><input id="version" v-model.trim="form.product_version" /></div><div class="field"><label for="client-path">客户端程序位置</label><input id="client-path" v-model.trim="form.client_executable" /></div></div>
            <div class="field"><label for="config-path">客户端配置文件</label><input id="config-path" v-model.trim="form.config_path" /></div>
          </fieldset>
          <button class="button" :disabled="busy || detecting || !configDirectory" @click="load(true)">检测此目录</button>
        </details>
        <div class="actions"><button class="button primary" :disabled="busy || detecting || !form.product_version || !form.client_executable || !form.config_path" @click="preview">{{ operation === 'preview' ? '正在准备方案…' : '准备接入方案' }}</button><button class="button" :disabled="busy || detecting" @click="installBackground">{{ operation === 'install' ? '正在安装后台…' : '安装本机后台' }}</button></div>
        <div aria-live="polite" :aria-busy="busy">
          <p v-if="busy" class="callout" role="status">{{ operation === 'install' ? '正在安装本机后台，请稍候，不要重复点击。' : operation === 'preview' ? '正在核对客户端并准备方案，请稍候。' : operation === 'apply' ? '正在应用接入并核对结果，请稍候。' : '正在处理，请稍候。' }}</p>
          <p v-if="error" class="callout error" role="alert">{{ error }}</p><p v-else-if="message" class="callout" role="status">{{ message }}</p>
        </div>
        <p v-if="!form.product_version || !form.client_executable || !form.config_path" class="hint">准备方案前需补齐：{{ [!form.product_version && '客户端版本', !form.client_executable && '程序位置', !form.config_path && '配置文件'].filter(Boolean).join('、') }}。可展开高级设置或重新检测。</p>
        <p class="hint">准备方案不修改客户端；只有应用才写入本工具条目。客户端信任仍由你确认。</p>
      </template>
    </section>
    <section v-if="plan" class="panel"><h2>确认接入</h2><p>为当前工具配置 {{ plan.events.length }} 个受支持事件。应用后可以继续选择其他工具。</p><div class="actions"><button class="button primary" :disabled="busy || detecting" @click="apply">{{ operation === 'apply' ? '正在应用…' : '应用接入' }}</button><button class="button" :disabled="busy" @click="plan = null">取消</button></div></section>
    <section v-if="applied" class="panel"><h2>接入结果</h2><div class="callout" role="status">{{ statusText[applied.status] ?? '请检查接入状态' }}。{{ applied.live_verified ? '已有入站记录；自然任务与实际收件仍需分别确认。' : '接入配置与真实收件分别验证；尚不能证明邮件已送达。' }}</div><button class="button primary" @click="emit('navigate', 'mail')">下一步：预览并测试邮件</button></section>
    <details class="panel"><summary>可选：接入遇到问题时获取 Agent 协助</summary><p class="muted small">正常接入无需协助包。检测或应用失败时，可生成脱敏诊断提示词，交给 Agent 排查。</p><button class="button" :disabled="busy" @click="createAssistance">生成排障协助提示词</button><div v-if="assistance"><p class="small">本机协助包：{{ assistancePath }}</p><button class="button" @click="copyAssistance">复制提示词</button><textarea readonly :value="assistance" rows="8" aria-label="排障协助提示词" style="width:100%" /></div></details>
  </div>
</template>
