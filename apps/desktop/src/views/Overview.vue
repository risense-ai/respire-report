<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import type { HealthReport } from '../generated/contracts'
import { bridge, errorText, type ControlReport } from '../services/bridge'
import { useUnsavedChanges } from '../composables/useUnsavedChanges'
const emit = defineEmits<{ changed: []; navigate: [page: 'mail' | 'integrations'] }>()
const health = ref<HealthReport | null>(null)
const service = ref<ControlReport | null>(null)
const message = ref('')
const error = ref('')
const busy = ref(false)
const setupKnown = ref(false); const mailReady = ref(false); const toolReady = ref(false)
const installation = ref<Awaited<ReturnType<typeof bridge.installationInfo>> | null>(null)
const targetDirectory = ref('')
const setupNeeded = computed(() => !setupKnown.value || !mailReady.value || !toolReady.value)
useUnsavedChanges({ dirty: () => !!targetDirectory.value, busy: () => busy.value, discard: () => { targetDirectory.value = '' } })
async function copyFirstUse() {
  if (!installation.value?.first_use_prompt) return
  try { await navigator.clipboard.writeText(installation.value.first_use_prompt); message.value = '新电脑使用提示词已复制，可交给该电脑上的 Agent 执行。' }
  catch { error.value = '复制失败，请展开提示词并手动复制。' }
}
async function chooseDirectory() {
  if (!installation.value?.can_choose_directory || !targetDirectory.value.trim()) return
  busy.value = true; error.value = ''
  try { await bridge.selectDataDirectory(targetDirectory.value.trim()); targetDirectory.value = '' }
  catch (e) { error.value = errorText(e) }
  finally { busy.value = false }
}
async function loadSetup() {
  const results = await Promise.allSettled([bridge.loadMail(), bridge.inspectIntegrations(), bridge.installationInfo()])
  if (results[0].status === 'fulfilled') mailReady.value = results[0].value.mail.has_secret && !!results[0].value.mail.recipient
  if (results[1].status === 'fulfilled') toolReady.value = results[1].value.clients.some(client => client.configured)
  if (results[2].status === 'fulfilled') installation.value = results[2].value
  setupKnown.value = results[0].status === 'fulfilled' && results[1].status === 'fulfilled'
}
async function refresh() {
  error.value = ''; service.value = null; health.value = null
  const results = await Promise.allSettled([bridge.health(), bridge.control('status')])
  if (results[0].status === 'fulfilled') health.value = results[0].value
  else error.value = errorText(results[0].reason)
  if (results[1].status === 'fulfilled') service.value = results[1].value
  else error.value = errorText(results[1].reason)
  emit('changed')
}
async function act(action: 'install' | 'start' | 'stop' | 'pause' | 'resume') {
  busy.value = true; message.value = ''; error.value = ''
  try { const result = await bridge.control(action); message.value = `${action === 'install' ? '后台安装' : action === 'start' ? '后台启动' : action === 'stop' ? '后台停止' : action === 'pause' ? '通知暂停' : '通知恢复'}：${result.code}`; await refresh() }
  catch (e) { error.value = errorText(e) }
  finally { busy.value = false }
}
onMounted(() => { void refresh(); void loadSetup() })
</script>
<template>
  <div>
    <p class="page-intro">查看通知是否在本机接收、排队与投递。关闭此窗口不会停止独立后台。</p>
    <div v-if="message" class="callout" role="status">{{ message }}</div><div v-if="error" class="callout error" role="alert">{{ error }}</div>
    <section v-if="setupNeeded" class="panel"><h2>首次使用</h2><p class="muted small">按三步完成设置。首次安装不需要迁移协助包，测试邮件会在预览确认后才发送。</p><div class="onboarding-steps"><button class="button" @click="emit('navigate', 'mail')">1. 配置邮箱{{ mailReady ? ' · 已保存' : '' }}</button><button class="button" :disabled="!mailReady" @click="emit('navigate', 'integrations')">2. 选择工具并接入{{ toolReady ? ' · 已登记' : '' }}</button><button class="button" :disabled="!mailReady || !toolReady" @click="emit('navigate', 'mail')">3. 预览并测试邮件</button></div></section>
    <details v-if="installation?.can_choose_directory" class="panel"><summary>首次安装：选择数据目录（可选）</summary><p class="muted small">默认目录通常无需调整。仅全新实例可以选择；这不会迁移已有安装或凭据。</p><p class="small">当前目录：{{ installation.data_dir }}</p><div class="form-row"><input v-model.trim="targetDirectory" aria-label="新实例的数据目录" placeholder="完整绝对路径" :disabled="busy" /><button class="button" :disabled="busy || !targetDirectory.trim()" @click="chooseDirectory">使用此目录</button></div></details>
    <details v-if="installation?.first_use_prompt" class="panel"><summary>新电脑使用：让 Agent 协助安装</summary><p class="muted small">可独立复制此提示词，无需本机迁移包。邮箱授权码仅在目标电脑私密输入。</p><button class="button" @click="copyFirstUse">复制新电脑使用提示词</button><details><summary>查看提示词</summary><textarea readonly :value="installation.first_use_prompt" rows="8" aria-label="新电脑使用提示词" style="width:100%" /></details></details>
    <div class="grid">
      <section class="panel"><h2>本机后台</h2><p class="muted small">{{ health?.daemon === 'running' ? '正在运行。已接入工具产生的事件会进入本机队列。' : '未确认运行。新事件可能暂存，实时邮件无法保证。' }}</p><div class="metric-row"><div><div class="metric">{{ health?.daemon === 'running' ? '运行中' : health?.daemon === 'stopped' ? '未运行' : '未确认' }}</div><div class="metric-label">当前状态</div></div><div><div class="metric">{{ !service ? '未确认' : service.installed ? '已安装' : '未安装' }}</div><div class="metric-label">登录后启动</div></div></div><div class="actions"><button v-if="service && !service.installed" class="button primary" :disabled="busy" @click="act('install')">安装本机后台</button><button v-if="service?.installed && health?.daemon !== 'running'" class="button primary" :disabled="busy" @click="act('start')">启动后台</button><button v-if="health?.daemon === 'running'" class="button" :disabled="busy" @click="act('pause')">暂停通知</button><button v-if="health?.daemon === 'running'" class="button" :disabled="busy" @click="act('resume')">恢复通知</button><button v-if="health?.daemon === 'running'" class="button danger" :disabled="busy" @click="act('stop')">停止后台</button><button class="button" :disabled="busy" @click="refresh">刷新状态</button></div></section>
      <section class="panel"><h2>最近投递流</h2><p class="muted small">这里展示真实队列计数，不推测收件箱是否收到。</p><ol class="timeline"><li><div class="timeline-copy"><strong>收到并排队</strong><small>{{ health?.queue_counts.queued ?? '—' }} 条等待处理</small></div></li><li><div class="timeline-copy"><strong>等待重试或认证</strong><small>{{ health?.queue_counts.retry_wait ?? '—' }} 条重试等待 · {{ health?.queue_counts.auth_blocked ?? '—' }} 条认证受阻</small></div></li><li><div class="timeline-copy"><strong>SMTP 已接受</strong><small>{{ health?.queue_counts.sent ?? '—' }} 条；仍需自行确认收件箱</small></div></li><li><div class="timeline-copy"><strong>投递结果不确定</strong><small>{{ health?.queue_counts.delivery_unknown ?? '—' }} 条；默认不会自动再发</small></div></li></ol></section>
    </div>
    <section v-if="health?.issues.length" class="panel"><h2>需要注意</h2><div class="list"><div v-for="issue in health.issues" :key="issue.code" class="list-row"><div><strong>{{ issue.message }}</strong><small class="mono">{{ issue.code }}</small></div></div></div></section>
  </div>
</template>
