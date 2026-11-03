<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { bridge, errorText, saveText, type MailView, type TestPreview, type TestReport } from '../services/bridge'
import { changedFieldLabels, useUnsavedChanges } from '../composables/useUnsavedChanges'
const emit = defineEmits<{ changed: []; navigate: [page: 'integrations'] }>()
const mail = ref<MailView>({ sender: '', recipient: '', provider: 'qq', host: 'smtp.qq.com', port: 465, tls: 'tls', username: '', has_secret: false })
const secret = ref('')
const baseline = ref('')
const revision = ref('missing')
const preview = ref<TestPreview | null>(null)
const testResult = ref<TestReport | null>(null)
const message = ref('')
const error = ref('')
const busy = ref(false)
const loaded = ref(false)
const loading = ref(false)
async function load() { loading.value = true; loaded.value = false; error.value = ''; try { const snapshot = await bridge.loadMail(); mail.value = snapshot.mail; revision.value = snapshot.revision; baseline.value = JSON.stringify(snapshot.mail); loaded.value = true } catch (e) { error.value = errorText(e) } finally { loading.value = false } }
const dirty = computed(() => loaded.value && (JSON.stringify(mail.value) !== baseline.value || secret.value.length > 0))
watch([mail, secret], () => { preview.value = null; testResult.value = null }, { deep: true })
const changes = computed(() => {
  const fields = baseline.value ? changedFieldLabels(mail.value, JSON.parse(baseline.value), {
    sender: '发件邮箱', recipient: '收件邮箱', provider: '邮箱服务商', host: 'SMTP 主机',
    port: 'SMTP 端口', tls: '加密方式', username: 'SMTP 用户名',
  }) : []
  if (secret.value.length) fields.push('授权码已修改')
  return fields
})
function discard() { if (baseline.value) mail.value = JSON.parse(baseline.value); secret.value = ''; preview.value = null }
const { requestAction } = useUnsavedChanges({ dirty: () => dirty.value, changes: () => changes.value, busy: () => busy.value || loading.value, save, discard })
async function save(): Promise<boolean> {
  if (!loaded.value || loading.value) return false
  busy.value = true; error.value = ''; message.value = ''; preview.value = null
  const input = { ...mail.value, secret: secret.value || null }
  try { const report = await bridge.saveMail(input, revision.value); message.value = saveText(report); if (!report.saved) return false; secret.value = ''; await load(); emit('changed'); return true }
  catch (e) { error.value = errorText(e); return false }
  finally { input.secret = null; busy.value = false }
}
async function showPreview() { if (dirty.value) { error.value = '请先保存邮箱修改，再预览测试邮件。'; return }; error.value = ''; testResult.value = null; try { preview.value = await bridge.previewTest() } catch (e) { error.value = errorText(e) } }
async function sendTest() {
  if (!preview.value) return
  const id = preview.value.confirmation; preview.value = null; busy.value = true; error.value = ''
  try { testResult.value = await bridge.sendTest(id); emit('changed') }
  catch (e) { error.value = errorText(e) }
  finally { busy.value = false }
}
onMounted(load)
</script>
<template>
  <div><p class="page-intro">先填写发件邮箱、授权码和收件邮箱。保存设置不会发送邮件；测试邮件需要另外预览并明确点击发送。</p>
    <div v-if="message" class="callout" role="status">{{ message }}</div><div v-if="error" class="callout error" role="alert">{{ error }}</div>
    <div v-if="!loaded" class="empty" role="status">{{ loading ? '正在读取邮箱设置…' : '邮箱设置尚未读取，暂不能编辑。' }} <button v-if="!loading" class="text-button" @click="load">重新读取</button></div>
    <div v-if="loaded" class="persistent-save"><span :class="dirty ? 'dirty-label' : 'muted'">{{ dirty ? `已修改：${changes.join('、')}` : '邮箱设置已保存' }}</span><div class="actions"><button class="button" :disabled="busy || loading" @click="requestAction(load)">重新载入</button><button class="button primary" :disabled="busy || loading || !dirty" @click="save">{{ busy ? '正在保存…' : '保存邮箱设置' }}</button></div></div>
    <section v-if="loaded" class="panel"><h2>发送邮件</h2><p class="muted small">授权码仅传给本机系统安全存储，不会显示已有授权码。</p>
      <fieldset class="settings-fields" :disabled="busy || loading"><div class="form-grid"><div class="field"><label for="provider">邮箱服务商</label><select id="provider" v-model="mail.provider"><option value="qq">QQ 邮箱</option><option value="163">163 邮箱</option><option value="gmail">Gmail</option><option value="custom">企业或其他 SMTP</option><option value="outlook" disabled>Outlook.com · OAuth 暂不可用</option><option value="microsoft365" disabled>Microsoft 365 · OAuth 暂不可用</option></select></div><div class="field"><label for="sender">发件邮箱</label><input id="sender" v-model.trim="mail.sender" type="email" autocomplete="off" placeholder="name@example.com" /></div><div class="field"><label for="secret">授权码{{ mail.has_secret ? '（留空保持原值）' : '' }}</label><input id="secret" v-model="secret" type="password" autocomplete="new-password" spellcheck="false" placeholder="在邮箱服务商处获取的授权码" /><span class="hint">请使用应用密码或授权码，不填写邮箱登录密码。</span></div><div class="field"><label for="recipient">接收邮箱</label><input id="recipient" v-model.trim="mail.recipient" type="email" autocomplete="off" placeholder="recipient@example.com" /></div></div>
      <details><summary>高级 SMTP 设置</summary><p class="muted small">仅自定义 SMTP 使用下列参数。加密连接必须使用 TLS 或 STARTTLS。</p><div class="form-grid"><div class="field"><label for="host">SMTP 主机</label><input id="host" v-model.trim="mail.host" :disabled="mail.provider !== 'custom'" /></div><div class="field"><label for="port">端口</label><input id="port" v-model.number="mail.port" type="number" min="1" max="65535" :disabled="mail.provider !== 'custom'" /></div><div class="field"><label for="tls">加密方式</label><select id="tls" v-model="mail.tls" :disabled="mail.provider !== 'custom'"><option value="tls">隐式 TLS</option><option value="starttls">STARTTLS</option></select></div><div class="field"><label for="username">SMTP 用户名</label><input id="username" v-model.trim="mail.username" placeholder="留空使用发件邮箱" /></div></div></details></fieldset>
      <div class="actions"><button class="button primary" :disabled="busy || loading || dirty || !mail.has_secret" @click="emit('navigate', 'integrations')">下一步：选择工具接入</button><button class="button" :disabled="busy || !loaded || loading || !mail.has_secret || dirty" @click="showPreview">预览测试邮件</button></div>
    </section>
    <section v-if="preview" class="panel"><h2>确认测试邮件</h2><p>从 <strong>{{ preview.sender }}</strong> 发往 <strong>{{ preview.recipient }}</strong></p><p>主题：{{ preview.subject }}</p><div class="callout warn">{{ preview.note }}</div><div class="actions"><button class="button primary" :disabled="busy" @click="sendTest">发送一次测试邮件</button><button class="button" @click="preview = null">取消</button></div></section>
    <section v-if="testResult" class="panel"><h2>本次测试结果</h2><div :class="['callout', testResult.smtp_accepted ? '' : 'warn']">{{ testResult.smtp_accepted ? 'SMTP 服务器已接受邮件。请在收件箱自行确认，当前无法证明已送达。' : testResult.outcome.outcome === 'uncertain' ? '投递结果不确定。系统不会自动重发；请先检查收件箱，再决定是否重新测试。' : `邮件未确认被 SMTP 接受：${testResult.outcome.outcome}` }}</div></section>
  </div>
</template>
