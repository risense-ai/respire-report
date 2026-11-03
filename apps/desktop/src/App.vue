<script setup lang="ts">
import { computed, onMounted, onUnmounted, provide, ref, shallowRef } from 'vue'
import { navigationGuardKey, type PageDraft } from './composables/useUnsavedChanges'
import type { Component } from 'vue'
import type { HealthReport } from './generated/contracts'
import { bridge, errorText } from './services/bridge'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import Overview from './views/Overview.vue'
import Mail from './views/Mail.vue'
import Integrations from './views/Integrations.vue'
import Rules from './views/Rules.vue'
import History from './views/History.vue'
import Diagnostics from './views/Diagnostics.vue'

type Page = 'overview' | 'mail' | 'integrations' | 'rules' | 'history' | 'diagnostics'
const pages: { id: Page; title: string; subtitle: string; component: Component }[] = [
  { id: 'overview', title: '总览', subtitle: '后台与投递状态', component: Overview },
  { id: 'mail', title: '邮箱设置', subtitle: '发件与测试', component: Mail },
  { id: 'integrations', title: '工具接入', subtitle: '检测与预演', component: Integrations },
  { id: 'rules', title: '通知规则', subtitle: '隐私与免打扰', component: Rules },
  { id: 'history', title: '通知记录', subtitle: '最近状态', component: History },
  { id: 'diagnostics', title: '诊断', subtitle: '问题与下一步', component: Diagnostics },
]
const icons: Record<Page, string> = { overview: 'M3 3h7v7H3z M14 3h7v7h-7z M3 14h7v7H3z M14 14h7v7h-7z', mail: 'M3 5h18v14H3z M3 6l9 7 9-7', integrations: 'M8 3v5 M16 3v5 M6 8h12v4a6 6 0 0 1-12 0z M12 18v3', rules: 'M4 6h16 M4 12h16 M4 18h16 M8 3v6 M16 9v6 M10 15v6', history: 'M4 7a9 9 0 1 1-1 7 M4 3v5h5 M12 7v5l3 2', diagnostics: 'M9 3h6 M12 3v4 M5 12a7 7 0 1 0 14 0a7 7 0 0 0-14 0 M12 9v4 M12 16h.01' }
const active = ref<Page>('overview')
const draft = shallowRef<PageDraft | null>(null)
const leaveDialog = ref<HTMLDialogElement | null>(null)
const pendingAction = shallowRef<(() => void | Promise<void>) | null>(null)
const leaving = shallowRef(false)
const leaveError = shallowRef('')
const canSave = computed(() => !!draft.value?.save)
const changedFields = computed(() => draft.value?.changes?.() ?? ['当前页面输入'])
function requestAction(action: () => void | Promise<void>) {
  if (leaving.value || pendingAction.value) return
  if (draft.value?.busy()) { runtimeMessage.value = '当前操作尚未完成，请稍候再离开。'; return }
  if (!draft.value?.dirty()) { void Promise.resolve().then(action).catch(e => { runtimeMessage.value = errorText(e) }); return }
  pendingAction.value = action; leaveError.value = ''; runtimeMessage.value = ''; leaveDialog.value?.showModal()
}
function cancelLeave() { if (leaving.value) return; pendingAction.value = null; leaveError.value = ''; leaveDialog.value?.close() }
async function resolveLeave(save: boolean) {
  if (!pendingAction.value || leaving.value) return
  leaving.value = true; leaveError.value = ''; runtimeMessage.value = ''
  try {
    if (save && (!draft.value?.save || !await draft.value.save())) { leaveError.value = '未能保存，草稿仍保留在当前页面。请继续编辑并检查提示。'; return }
    if (!save) draft.value?.discard()
    const action = pendingAction.value; pendingAction.value = null; leaveDialog.value?.close(); await action()
  } catch {
    if (leaveDialog.value?.open) leaveError.value = '操作未完成，草稿仍保留。可继续编辑后重试。'
    else runtimeMessage.value = '离开操作未完成，请重试。'
  }
  finally { leaving.value = false }
}
function navigate(page: Page) { if (page !== active.value) requestAction(() => { active.value = page }) }
provide(navigationGuardKey, {
  register(value) { draft.value = value; return () => { if (draft.value === value) draft.value = null } },
  request: requestAction,
})

const health = ref<HealthReport | null>(null)
const healthError = ref(false)
const desktopInfo = ref<Awaited<ReturnType<typeof bridge.desktopInfo>> | null>(null)
const runtimeMessage = ref('')
const trayNotice = ref('')
const unlisteners: UnlistenFn[] = []
let disposed = false
async function loadDesktopInfo() {
  try { desktopInfo.value = await bridge.desktopInfo(); if (desktopInfo.value.runtime_issue) runtimeMessage.value = errorText(desktopInfo.value.runtime_issue) }
  catch (e) { runtimeMessage.value = errorText(e) }
}
async function subscribeToTray() {
  const registrations = await Promise.allSettled([
    listen('agentnotify-close-requested', () => requestAction(() => bridge.confirmClose())),
    listen('agentnotify-quit-requested', () => requestAction(() => bridge.confirmQuit())),
    listen('agentnotify-config-changed', () => { trayNotice.value = '托盘已更新通知规则。当前未保存修改已保留；重新载入会丢弃它们，请先决定是否保留。'; void refreshHealth() }),
    listen<{ code: string }>('agentnotify-runtime-error', event => { runtimeMessage.value = errorText(event.payload.code) }),
  ])
  for (const registration of registrations) {
    if (registration.status === 'fulfilled') { if (disposed) registration.value(); else unlisteners.push(registration.value) }
    else runtimeMessage.value = '无法接收托盘状态变化，请重新打开设置窗口。'
  }
  if (!disposed && registrations.every(result => result.status === 'fulfilled')) {
    try { await bridge.desktopReady() } catch (e) { runtimeMessage.value = errorText(e) }
  }
}
let timer: ReturnType<typeof setInterval> | undefined
async function refreshHealth() {
  try { health.value = await bridge.health(); healthError.value = false }
  catch { healthError.value = true }
}
onMounted(() => { void refreshHealth(); void loadDesktopInfo(); void subscribeToTray(); timer = setInterval(refreshHealth, 15000) })
onUnmounted(() => { disposed = true; if (timer) clearInterval(timer); for (const unlisten of unlisteners) unlisten() })
</script>

<template>
  <dialog ref="leaveDialog" class="unsaved-dialog" @cancel.prevent="cancelLeave">
    <h2>有未保存的修改</h2><p>{{ canSave ? '保存后离开，或放弃本页修改。' : '本页输入尚未应用，离开将放弃这些修改。' }}继续编辑会保留当前内容。</p>
    <p class="small">修改项：{{ changedFields.join('、') }}</p>
    <p v-if="leaveError" class="callout error" role="alert">{{ leaveError }}</p>
    <div class="actions"><button class="button" :disabled="leaving" autofocus @click="cancelLeave">继续编辑</button><button class="button" :disabled="leaving" @click="resolveLeave(false)">放弃并离开</button><button v-if="canSave" class="button primary" :disabled="leaving" @click="resolveLeave(true)">{{ leaving ? '保存中…' : '保存并离开' }}</button></div>
  </dialog>
  <div class="shell">
    <aside class="sidebar">
      <div class="brand"><span class="brand-mark" aria-hidden="true">A</span><div><strong>AgentNotify</strong><small>本机任务邮件通知</small></div></div>
      <nav aria-label="主要页面">
        <button v-for="page in pages" :key="page.id" class="nav-item" :class="{ selected: active === page.id }" :aria-current="active === page.id ? 'page' : undefined" @click="navigate(page.id)">
          <svg class="nav-icon" viewBox="0 0 24 24" aria-hidden="true"><path :d="icons[page.id]" /></svg><span>{{ page.title }}</span>
        </button>
      </nav>
      <p class="sidebar-note">任务结束，及时知晓。<br />关闭窗口后可从系统托盘打开。</p>
    </aside>
    <div class="workspace">
      <header class="topbar">
        <div><span class="eyebrow">设置 / {{ pages.find(p => p.id === active)?.title }}</span><h1>{{ pages.find(p => p.id === active)?.title }}</h1></div>
        <div class="backend-state" :class="health?.daemon === 'running' ? 'is-running' : 'is-attention'" role="status">
          <span class="state-dot" aria-hidden="true"></span>
          {{ !health || healthError || health?.daemon === 'unknown' || health?.daemon === 'unavailable' ? '后台状态未确认' : health?.daemon === 'running' ? '后台运行中' : '后台未运行' }}
        </div>
      </header>
      <main><div v-if="runtimeMessage" class="callout error" role="alert">{{ runtimeMessage }} <button class="text-button" @click="runtimeMessage = ''">收起</button></div><div v-if="trayNotice" class="callout" role="status">{{ trayNotice }} <button class="text-button" @click="trayNotice = ''">知道了</button></div><component :is="pages.find(p => p.id === active)?.component" :key="active" @changed="refreshHealth" @navigate="navigate" /></main>
      <footer class="footer"><span class="app-version">AgentNotify {{ desktopInfo ? `v${desktopInfo.version} · ${desktopInfo.build_id}` : '· 版本未读取' }}</span><details v-if="desktopInfo" class="runtime-details"><summary>运行信息</summary><div class="runtime-popover"><p><strong>程序位置</strong><span class="mono">{{ desktopInfo.executable }}</span></p><p><strong>数据位置</strong><span class="mono">{{ desktopInfo.data_dir }}</span></p></div></details></footer>
    </div>
  </div>
</template>
