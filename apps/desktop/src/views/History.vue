<script setup lang="ts">
import { onMounted, ref, shallowRef } from 'vue'
import { bridge, errorText, type HistoryRow } from '../services/bridge'
const rows = ref<HistoryRow[]>([])
const error = shallowRef('')
const loading = shallowRef(false)
const expandedId = shallowRef<string | null>(null)
const statusText: Record<string, string> = { queued:'等待投递', leased:'正在处理', sent:'SMTP 已接受', suppressed:'按规则跳过', digest_pending:'等待汇总', retry_wait:'稍后重试', auth_blocked:'认证受阻', delivery_unknown:'结果不确定', expired:'已过期', permanent_failed:'无法投递' }
const kindText: Record<string, string> = { response_finished:'本轮处理结束', attention_required:'等待输入', execution_failed:'执行失败', interrupted:'已中断', task_completed:'任务完成', stalled:'长时间未结束' }
const stageText: Record<string, string> = { prepared:'准备发送', connecting:'连接邮件服务器', envelope:'提交发件人与收件人', data_started:'已开始提交邮件内容', accepted:'邮件服务器已接受' }
const reasonText: Record<string, string> = { queued:'符合接收规则，已入队', user_policy:'按通知设置跳过', quiet_receive:'接收时处于免打扰时段', presence_receive:'接收时电脑活跃或空闲状态未知', duplicate:'重复事件', aggregated:'已合并到另一条汇总通知' }
const diagnosticText: Record<string, string> = { sequence_gap:'事件序号存在间隔', late_sequence:'事件到达顺序晚于已处理记录', order_unknown:'事件顺序无法确认' }
const sourceText: Record<string, string> = { codex:'Codex', claude_code:'Claude Code', workbuddy:'WorkBuddy', generic:'通用接入' }
async function load() {
  loading.value = true; error.value = ''
  try {
    rows.value = await bridge.history()
    if (!rows.value.some(row => row.id === expandedId.value)) expandedId.value = null
  } catch (e) { error.value = errorText(e) }
  finally { loading.value = false }
}
function time(value: number | string | null) {
  if (value === null || value === '') return '未记录'
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? '未记录' : date.toLocaleString('zh-CN')
}
function codedLabel(code: string | null, labels: Record<string, string>) {
  return code ? (labels[code] ?? `其他记录（${code}）`) : '未记录'
}
function statusClass(status: string) {
  return ['delivery_unknown','auth_blocked','permanent_failed'].includes(status) ? 'warn' : status === 'sent' ? 'good' : ''
}
function deliveryNote(status: string) {
  if (status === 'sent') return '邮件服务器已接受，不代表已进入收件箱。'
  if (status === 'delivery_unknown') return '投递结果不确定，请先核对收件箱；系统不会自动重发。'
  if (['auth_blocked', 'permanent_failed', 'retry_wait'].includes(status)) return '详情只显示已保存的状态与诊断；未保存具体 SMTP 错误时无法补查原因。'
  if (status === 'suppressed') return '此条通知已按规则跳过，不会补发。'
  return '处理阶段是最近保存的投递进度，不是完整操作日志。'
}
function bodyNote(status: string) {
  return ['sent','suppressed','delivery_unknown','expired','permanent_failed','digest_pending'].includes(status)
    ? '此状态下的问答正文按隐私规则清理；详情无法恢复正文，请在原会话或已收到的邮件中查看。'
    : '详情仅展示通知元数据，不读取暂存的问答正文，也不延长正文留存。'
}
onMounted(load)
</script>

<template>
  <div>
    <p class="page-intro">最近 40 条通知。点击“查看详情”了解项目、会话和投递情况；详情不展示问答正文。</p>
    <div v-if="error" class="callout error" role="alert">{{ error }}</div>
    <section class="panel">
      <div class="history-header"><h2>通知记录</h2><button class="button" :disabled="loading" @click="load">{{ loading ? '刷新中…' : '刷新' }}</button></div>
      <div v-if="!rows.length" class="empty">暂无可显示的通知。接入工具并产生受支持事件后，这里会显示真实状态。</div>
      <div v-else class="table-wrap">
        <table>
          <thead><tr><th>接收时间</th><th>来源工具</th><th>项目 / 会话</th><th>事件</th><th>状态</th><th>操作</th></tr></thead>
          <tbody>
            <template v-for="row in rows" :key="row.id">
              <tr>
                <td>{{ time(row.received_at) }}</td>
                <!-- 来源工具取通知记录的来源编码，列表与详情共用名称，缺失时显示未记录。 -->
                <td>{{ codedLabel(row.source, sourceText) }}</td>
                <td class="history-title"><strong>{{ row.project || '项目未记录' }}</strong><span>{{ row.session || '会话未记录' }}</span></td>
                <td>{{ kindText[row.kind] ?? '未知事件' }}</td>
                <td><span :class="['status-chip', statusClass(row.status)]">{{ statusText[row.status] ?? '未知状态' }}</span></td>
                <td><button class="button detail-toggle" :aria-expanded="expandedId === row.id" :aria-controls="`notice-${row.id}`" @click="expandedId = expandedId === row.id ? null : row.id">{{ expandedId === row.id ? '收起详情' : '查看详情' }}</button></td>
              </tr>
              <tr v-if="expandedId === row.id" class="detail-row">
                <td colspan="6">
                  <section :id="`notice-${row.id}`" class="notice-detail" aria-label="通知详情">
                    <dl class="detail-grid">
                      <div><dt>项目</dt><dd>{{ row.project || '未记录' }}</dd></div>
                      <div><dt>会话</dt><dd>{{ row.session || '未记录' }}</dd></div>
                      <div><dt>来源工具</dt><dd>{{ codedLabel(row.source, sourceText) }}</dd></div>
                      <div><dt>事件</dt><dd>{{ kindText[row.kind] ?? '未知事件' }}</dd></div>
                      <div><dt>发生时间</dt><dd>{{ time(row.occurred_at) }}</dd></div>
                      <div><dt>本机接收时间</dt><dd>{{ time(row.received_at) }}</dd></div>
                      <div><dt>当前状态</dt><dd>{{ statusText[row.status] ?? '未知状态' }}</dd></div>
                      <div><dt>投递尝试次数</dt><dd>{{ row.attempts }} 次</dd></div>
                      <div><dt>最近投递阶段</dt><dd>{{ codedLabel(row.send_stage, stageText) }}</dd></div>
                      <div><dt>接收规则记录</dt><dd>{{ codedLabel(row.policy_reason, reasonText) }}</dd></div>
                    </dl>
                    <p class="detail-note">{{ deliveryNote(row.status) }}</p>
                    <div class="detail-diagnostics"><strong>已记录的诊断</strong><ul v-if="row.diagnostics.length"><li v-for="(code, index) in row.diagnostics" :key="`${index}-${code}`">{{ codedLabel(code, diagnosticText) }}</li></ul><p v-else>未记录具体诊断信息。</p></div>
                    <p class="detail-note">{{ bodyNote(row.status) }}</p>
                  </section>
                </td>
              </tr>
            </template>
          </tbody>
        </table>
      </div>
    </section>
  </div>
</template>

<style scoped>
.history-header { display: flex; justify-content: space-between; align-items: center; gap: 12px; }
.history-title { max-width: 300px; overflow-wrap: anywhere; }
.history-title strong, .history-title span { display: block; }
.history-title span { margin-top: 4px; color: #637188; font-size: 12px; }
.detail-toggle { white-space: nowrap; }
.detail-row > td { padding: 0; }
.notice-detail { padding: 20px; background: #f6f7fb; border-left: 3px solid #7161db; }
.detail-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px 24px; margin: 0; }
.detail-grid dt { font-size: 12px; color: #637188; margin-bottom: 4px; }
.detail-grid dd { margin: 0; overflow-wrap: anywhere; }
.detail-note { color: #637188; font-size: 12px; line-height: 1.7; margin: 16px 0 0; }
.detail-diagnostics { margin-top: 16px; font-size: 13px; }
.detail-diagnostics p, .detail-diagnostics ul { margin: 6px 0 0; }
@media (max-width: 720px) { .detail-grid { grid-template-columns: 1fr; } }
</style>
