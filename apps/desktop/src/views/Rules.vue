<script setup lang="ts">
import { computed, onMounted, ref, toRaw } from 'vue'
import { changedFieldLabels, useUnsavedChanges } from '../composables/useUnsavedChanges'
import type { ConfigV1 } from '../generated/contracts'
import { bridge, errorText, saveText, type ConfigSnapshot } from '../services/bridge'
const emit = defineEmits<{ changed: [] }>()
const snapshot = ref<ConfigSnapshot | null>(null)
const config = ref<ConfigV1 | null>(null)
const message = ref(''); const error = ref(''); const busy = ref(false); const loading = ref(false)
// 通知模式同时映射启用开关和离席策略，关闭时保留原离席偏好。
const notificationMode = computed({
  get: () => !config.value?.enabled ? 'off' : config.value.presence.mode,
  set: (mode: string) => { if (!config.value) return; config.value.enabled = mode !== 'off'; if (mode === 'always' || mode === 'away_only') config.value.presence.mode = mode },
})
async function load() {
  loading.value = true; error.value = ''; config.value = null; snapshot.value = null
  try { const loaded = await bridge.loadConfig(); config.value = structuredClone(loaded.config); snapshot.value = loaded }
  catch (e) { error.value = errorText(e) }
  finally { loading.value = false }
}
const dirty = computed(() => !!config.value && !!snapshot.value && JSON.stringify(config.value) !== JSON.stringify(snapshot.value.config))
const changes = computed(() => changedFieldLabels(config.value, snapshot.value?.config, {
  enabled: '通知模式', 'presence.mode': '通知模式', 'presence.idle_seconds': '电脑空闲时间',
  include_question: '包含提问正文', include_answer: '包含回答正文',
  'policy.notify_subagents': '子代理通知', 'policy.notify_attention': '等待输入通知',
  'quiet.enabled': '免打扰开关', 'quiet.start': '免打扰开始时间', 'quiet.end': '免打扰结束时间',
  'quiet.behavior': '静默期间处理方式', 'quiet.timezone': '时区',
  'watchdog.enabled': '长时间未结束提醒', 'watchdog.mode': '观察方式', 'watchdog.threshold_seconds': '观察阈值',
  'policy.aggregate_finishes': '短时间结束聚合', 'policy.suppress_internal_success': '内部后台事件过滤',
  metadata_retention_days: '记录保留天数', queue_ttl_hours: '队列最长保留时间',
}))
function discard() { if (snapshot.value) config.value = structuredClone(toRaw(snapshot.value.config)) }
const { requestAction } = useUnsavedChanges({ dirty: () => dirty.value, changes: () => changes.value, busy: () => busy.value || loading.value, save, discard })
async function save(): Promise<boolean> {
  if (!config.value || !snapshot.value || loading.value) return false
  busy.value = true; error.value = ''; message.value = ''
  try { const report = await bridge.saveConfig(config.value, snapshot.value.revision); message.value = saveText(report); if (!report.saved) return false; await load(); emit('changed'); return true }
  catch (e) { error.value = errorText(e); return false }
  finally { busy.value = false }
}
onMounted(load)
</script>
<template>
  <div class="rules-page">
    <p class="page-intro">决定什么时候通知，以及邮件中包含什么。修改后保存生效。</p>
    <div v-if="message" class="callout" role="status">{{ message }}</div>
    <div v-if="error" class="callout error" role="alert">{{ error }} <button class="text-button" :disabled="busy || loading" @click="requestAction(load)">重新载入</button></div>
    <p v-if="loading" class="empty" role="status">正在读取通知设置…</p>
    <template v-if="config">
      <div class="persistent-save" role="region" aria-label="保存通知规则"><span :class="dirty ? 'dirty-label' : 'muted'">{{ dirty ? `已修改：${changes.join('、') || '通知设置'}` : '设置已保存' }}</span><div class="actions"><button class="button" :disabled="busy || loading" @click="requestAction(load)">重新载入</button><button class="button primary" :disabled="busy || loading || !dirty" @click="save">{{ busy ? '正在保存…' : '保存通知规则' }}</button></div></div>
      <fieldset class="settings-fields" :disabled="busy || loading">
        <section class="panel mode-panel">
          <div class="section-heading"><h2>通知时机</h2><span class="status-chip">{{ config.enabled ? '通知已开启' : '通知已关闭' }}</span></div>
          <div class="form-grid">
            <div class="field"><label for="presence-mode">通知模式</label><select id="presence-mode" v-model="notificationMode"><option value="off">关闭通知</option><option value="always">始终通知</option><option value="away_only">仅电脑空闲或锁屏时通知</option></select></div>
            <div class="field"><label for="idle-seconds">电脑空闲时间（秒）</label><input id="idle-seconds" v-model.number="config.presence.idle_seconds" type="number" min="1" max="86400" step="1" /><span class="hint">仅空闲模式使用；关闭通知时仍保留此参数。</span></div>
          </div>
          <p class="hint">任务结束时已空闲或锁屏才接收通知；活跃或状态未知则跳过。已接收通知发送前若恢复活跃，最多等至接收后上述秒数，逾期跳过。始终通知仍遵守免打扰。</p>
        </section>
        <div class="grid rules-grid">
          <section class="panel"><h2>邮件内容</h2>
            <label class="switch-row"><span><strong>包含提问正文</strong><small>开启后以加密方式暂存正文。</small></span><input v-model="config.include_question" type="checkbox" role="switch" /></label>
            <label class="switch-row"><span><strong>包含回答正文</strong><small>投递结束后清除暂存正文。</small></span><input v-model="config.include_answer" type="checkbox" role="switch" /></label>
            <label class="switch-row"><span><strong>保留子代理通知</strong><small>不根据回答内容猜测来源。</small></span><input v-model="config.policy.notify_subagents" type="checkbox" role="switch" /></label>
            <label class="switch-row"><span><strong>等待输入时通知</strong><small>需要客户端提供对应事件。</small></span><input v-model="config.policy.notify_attention" type="checkbox" role="switch" /></label>
          </section>
          <section class="panel"><div class="section-heading"><h2>免打扰</h2><label class="compact-switch"><span>启用</span><input v-model="config.quiet.enabled" type="checkbox" role="switch" aria-label="启用免打扰" /></label></div>
            <div class="form-grid"><div class="field"><label for="quiet-start">开始时间</label><input id="quiet-start" v-model="config.quiet.start" type="time" /></div><div class="field"><label for="quiet-end">结束时间</label><input id="quiet-end" v-model="config.quiet.end" type="time" /></div><div class="field"><label for="quiet-mode">静默期间的通知</label><select id="quiet-mode" v-model="config.quiet.behavior"><option value="skip">跳过，不补发</option><option value="digest">汇总到下一时段</option></select></div><div class="field"><label for="timezone">时区</label><input id="timezone" v-model.trim="config.quiet.timezone" placeholder="system" /></div></div>
            <p class="hint">system 表示系统时区。免打扰期间不发送即时邮件。</p>
            <div v-if="config.quiet.enabled && config.quiet.start === config.quiet.end" class="callout warn">开始与结束相同表示全天静默。</div>
          </section>
        </div>
        <details class="panel advanced-panel"><summary>任务观察与更多设置 <span class="summary-note">{{ config.watchdog.enabled ? '观察已开启' : '观察未启用' }} · 保留 {{ config.watchdog.threshold_seconds }} 秒阈值</span></summary>
          <p class="muted small">长时间未结束只表示未收到结束信号，不等同于任务卡住。</p>
          <label class="switch-row"><span><strong>长时间未结束提醒</strong><small>{{ snapshot?.watchdog_ready ? '已登记工具具备可靠开始事件。' : '当前工具缺少可靠开始信号，暂不能开启；阈值仍会保存。' }}</small></span><input v-model="config.watchdog.enabled" type="checkbox" role="switch" :disabled="!snapshot?.watchdog_ready" /></label>
          <div class="form-grid"><div class="field"><label for="watchdog-mode">观察方式</label><select id="watchdog-mode" v-model="config.watchdog.mode"><option value="elapsed_without_finish">开始后仍未结束</option><option value="inactivity" :disabled="!snapshot?.inactivity_ready">长时间无进展{{ snapshot?.inactivity_ready ? '' : ' · 暂不可用' }}</option></select></div><div class="field"><label for="threshold">阈值（秒）</label><input id="threshold" v-model.number="config.watchdog.threshold_seconds" type="number" min="60" max="86400" /></div></div>
          <label class="switch-row"><span><strong>聚合短时间内的正常结束</strong><small>关闭后仍保留 3 秒稳定等待。</small></span><input v-model="config.policy.aggregate_finishes" type="checkbox" role="switch" /></label>
          <label class="switch-row"><span><strong>过滤已证实的内部后台成功事件</strong><small>未知来源和普通 JSON 回答不因此被过滤。</small></span><input v-model="config.policy.suppress_internal_success" type="checkbox" role="switch" /></label>
          <div class="form-grid"><div class="field"><label for="metadata-days">记录保留天数</label><input id="metadata-days" v-model.number="config.metadata_retention_days" type="number" min="1" /></div><div class="field"><label for="queue-hours">队列最长保留（小时）</label><input id="queue-hours" v-model.number="config.queue_ttl_hours" type="number" min="1" max="24" /></div></div>
        </details>
      </fieldset>

    </template>
  </div>
</template>
