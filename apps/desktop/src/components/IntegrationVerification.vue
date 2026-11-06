<script setup lang="ts">
import { computed, onUnmounted, shallowRef } from 'vue'
import { bridge, errorText, type LiveVerificationCursor, type LiveVerificationEvent } from '../services/bridge'

const { instanceId, disabled } = defineProps<{ instanceId: string; disabled: boolean }>()

const phase = shallowRef<'idle' | 'starting' | 'watching' | 'finished'>('idle')
const channel = shallowRef<LiveVerificationCursor['channel']>()
const evidence = shallowRef<LiveVerificationEvent | null>(null)
const failure = shallowRef('')
const active = computed(() => phase.value === 'starting' || phase.value === 'watching')
const source = computed(() => channel.value === 'local_observer' ? '本机只读观察' : '客户端 Hook')

const statusText: Record<string, string> = {
  queued: '已入队，等待投递',
  leased: '已入队，正在投递',
  suppressed: '已收到，按通知规则跳过',
  digest_pending: '已收到，等待汇总',
  retry_wait: '已入队，等待重试',
  auth_blocked: '已入队，邮箱认证受阻',
  delivery_unknown: '已收到，投递结果不确定',
  expired: '已收到，通知已过期',
  permanent_failed: '已收到，邮件无法投递',
}
const terminal = new Set(['suppressed', 'digest_pending', 'auth_blocked', 'delivery_unknown', 'expired', 'permanent_failed'])
const resultText = computed(() => {
  if (!evidence.value) return ''
  if (evidence.value.smtp_accepted) return 'SMTP 已接受，请自行确认收件箱'
  return statusText[evidence.value.status] ?? '已收到，SMTP 接受尚未确认'
})

let run = 0
let timer: number | undefined

/** 取消当前查询及后续刷新；已发出的只读调用完成后不再写界面。 */
function stop() {
  run += 1
  window.clearTimeout(timer)
  phase.value = 'idle'
}

/** 开启一次验证，轮询同一接入对象的真实完成事件及投递状态。 */
async function start() {
  // 1. 开启新窗口并清空上一次结果。
  const current = ++run
  phase.value = 'starting'
  failure.value = ''
  evidence.value = null
  try {
    const cursor = await bridge.startIntegrationVerification(instanceId)
    if (current !== run) return
    channel.value = cursor.channel
    phase.value = 'watching'
    const deadline = performance.now() + 120_000

    // 2. 只有终态或两分钟到期才结束，等待时可随时取消。
    const poll = async () => {
      try {
        const event = await bridge.pollIntegrationVerification(cursor)
        if (current !== run) return
        evidence.value = event
        if ((event && (event.smtp_accepted || terminal.has(event.status))) || performance.now() >= deadline) {
          phase.value = 'finished'
          return
        }
        timer = window.setTimeout(poll, 1500)
      } catch (error) {
        if (current !== run) return
        failure.value = errorText(error)
        phase.value = 'finished'
      }
    }
    await poll()
  } catch (error) {
    if (current !== run) return
    failure.value = errorText(error)
    phase.value = 'finished'
  }
}
onUnmounted(stop)
</script>

<template>
  <div>
    <div class="actions">
      <button type="button" class="button" :disabled="disabled || active" @click="start">
        {{ active ? '正在验证…' : '验证当前接入' }}
      </button>
      <button v-if="active" type="button" class="button" @click="stop">结束验证</button>
    </div>
    <p v-if="failure" class="callout error" role="alert">{{ failure }}</p>
    <p v-else-if="evidence" class="callout" role="status">
      {{ source }} · {{ new Date(evidence.received_at).toLocaleString('zh-CN') }} · {{ resultText }}
    </p>
    <p v-else-if="phase === 'watching'" class="callout" role="status">
      正在等待{{ source }}。请在所选客户端完成一次普通问答。验证只跟踪该客户端的第一条新完成记录，两分钟后自动结束。本操作只观察通知记录，不会额外发送测试邮件。
    </p>
    <p v-else-if="phase === 'finished'" class="callout" role="status">本次验证未查到新的完成记录。请确认任务已结束，并检查客户端接入和本机后台状态。</p>
  </div>
</template>
