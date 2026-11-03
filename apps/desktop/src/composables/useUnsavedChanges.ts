import { inject, onMounted, onUnmounted, type InjectionKey } from 'vue'

export interface PageDraft {
  dirty: () => boolean
  changes?: () => string[]
  busy: () => boolean
  save?: () => Promise<boolean>
  discard: () => void
}
export interface NavigationGuard {
  register: (draft: PageDraft) => () => void
  request: (action: () => void | Promise<void>) => void
}
export const navigationGuardKey: InjectionKey<NavigationGuard> = Symbol('page-draft-navigation')

/** 草稿只留在当前页面内存；应用壳负责确认离开，密码不进入全局状态。 */
export function useUnsavedChanges(draft: PageDraft) {
  const navigation = inject(navigationGuardKey)
  let unregister: (() => void) | undefined
  onMounted(() => { unregister = navigation?.register(draft) })
  onUnmounted(() => unregister?.())
  return { requestAction: (action: () => void | Promise<void>) => navigation ? navigation.request(action) : void action() }
}

/** 只返回中文字段名，不把邮箱、路径或授权码值送到确认弹窗。 */
export function changedFieldLabels(current: unknown, baseline: unknown, labels: Record<string, string>): string[] {
  const read = (value: unknown, path: string): unknown => path.split('.').reduce<unknown>((item, key) =>
    item !== null && typeof item === 'object' ? (item as Record<string, unknown>)[key] : undefined, value)
  return [...new Set(Object.entries(labels).filter(([path]) =>
    JSON.stringify(read(current, path)) !== JSON.stringify(read(baseline, path))).map(([, label]) => label))]
}
