/**
 * Which model answers, and how hard it thinks.
 *
 * One selector for the whole app: the main composer and the code chat beside a
 * page show the same pill, open the same card and list the same providers, so a
 * model picked in one place reads the same in the other. A model id alone does
 * not name a model — two providers can serve the same id — so a choice is
 * always a provider and a model together.
 */
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import clsx from 'clsx'
import {
  IconCheckOutlineRegular,
  IconChevronDownOutlineRegular,
  IconChevronLeftOutlineRegular,
  IconChevronRightOutlineRegular,
  Switch,
} from '@deepseek-ai/dsh-client-ui-primitives'
import { availableEfforts, resolveEffort } from '../../reasoningEffort.ts'
import type { ProviderConfig } from '../../api.ts'
import css from './ModelSelect.module.css'

/** How a thinking level is written, short enough for the pill and clear in the card. */
const EFFORT_WORDS: Readonly<Record<string, string>> = {
  '': '默认',
  none: '关闭',
  low: '轻量',
  medium: '中等',
  high: '深度',
  xhigh: '超深度',
  max: '最大',
}

const effortWord = (effort: string): string => EFFORT_WORDS[effort] ?? effort
const effortPhrase = (effort: string): string => (effort === '' ? '默认' : effortWord(effort) + ' (' + effort + ')')

export function ModelSelect({
  providers,
  providerId,
  modelName,
  reasoningEffort,
  fastMode = false,
  disabled = false,
  onSelect,
  onSetGenerationOptions,
  onOpenSettings,
  onOpenChange,
  className,
  placeholder = '选择模型',
}: {
  readonly providers: readonly ProviderConfig[]
  /** The provider offering the chosen model; without it the first one that has it wins. */
  readonly providerId?: string | null | undefined
  readonly modelName: string
  readonly reasoningEffort?: string | null | undefined
  readonly fastMode?: boolean | undefined
  readonly disabled?: boolean | undefined
  readonly onSelect?: ((providerId: string, modelId: string) => Promise<void> | void) | undefined
  readonly onSetGenerationOptions?: ((effort: string, fastMode: boolean) => Promise<void> | void) | undefined
  readonly onOpenSettings?: (() => void) | undefined
  readonly onOpenChange?: ((open: boolean) => void) | undefined
  readonly className?: string | undefined
  /** What the pill says before a model is chosen. */
  readonly placeholder?: string | undefined
}) {
  const [open, setOpen] = useState(false)
  const [pane, setPane] = useState<'root' | 'model' | 'effort'>('root')
  const [pos, setPos] = useState<{ left: number; top?: number; bottom?: number }>({ left: 24 })
  const trigger = useRef<HTMLButtonElement>(null)
  const menu = useRef<HTMLDivElement>(null)

  useLayoutEffect(() => { onOpenChange?.(open) }, [open, onOpenChange])

  // A card that stays open while the pointer is somewhere else is a card in the way.
  useEffect(() => {
    if (!open) return
    const away = (event: PointerEvent) => {
      const target = event.target as Node | null
      if (target !== null && !trigger.current?.contains(target) && !menu.current?.contains(target)) setOpen(false)
    }
    const escape = (event: KeyboardEvent) => { if (event.key === 'Escape') setOpen(false) }
    document.addEventListener('pointerdown', away)
    window.addEventListener('keydown', escape)
    return () => {
      document.removeEventListener('pointerdown', away)
      window.removeEventListener('keydown', escape)
    }
  }, [open])

  const activeProvider = providers.find(p => p.id === providerId && p.models.includes(modelName))
    ?? providers.find(p => p.models.includes(modelName))
    ?? providers.find(p => p.id === providerId)
  // Two providers under one display name are told apart by id, or the list lies.
  const label = (provider: ProviderConfig): string => {
    const name = provider.display_name || provider.id
    return providers.some(other => other.id !== provider.id && (other.display_name || other.id) === name)
      ? name + '（' + provider.id + '）'
      : name
  }
  const shared = providers.filter(p => p.models.includes(modelName)).length > 1
  const modelLabel = modelName !== '' && shared && activeProvider !== undefined
    ? modelName + ' · ' + label(activeProvider)
    : modelName
  const shown = modelLabel === '' ? placeholder : modelLabel

  const capability = (activeProvider !== undefined && modelName !== '' ? activeProvider.model_capabilities[modelName] : undefined)
  const efforts = capability?.reasoning_efforts ?? []
  const current = resolveEffort(reasoningEffort ?? null, efforts, capability?.default_effort ?? null)
  const currentWord = current === '' ? undefined : effortWord(current)
  const effortOptions = useMemo(() => availableEfforts(efforts).map(effort => ({ effort, label: effortPhrase(effort) })), [efforts])

  /**
   * Hang the card under the trigger, aligned to its right edge like a menu, but
   * measure it first: a card opened near the left of a wide window would hang
   * off the screen. It goes above the trigger when there is no room below.
   */
  useLayoutEffect(() => {
    if (!open) return
    const card = menu.current
    const rect = trigger.current?.getBoundingClientRect()
    if (card === null || rect === undefined) return
    const gap = 8
    const width = card.offsetWidth
    const left = Math.min(Math.max(gap, rect.right - width), Math.max(gap, window.innerWidth - width - gap))
    const below = rect.bottom + gap
    if (below + card.offsetHeight <= window.innerHeight - gap) setPos({ left, top: below })
    else setPos({ left, bottom: window.innerHeight - rect.top + gap })
  }, [open, pane, providers, effortOptions.length])

  const openMenu = () => {
    if (open) { setOpen(false); return }
    setPane('root')
    setOpen(true)
  }

  const choose = (provider: ProviderConfig, model: string) => {
    setOpen(false)
    setPane('root')
    void onSelect?.(provider.id, model)
  }

  return (
    <>
      <button
        ref={trigger}
        type="button"
        className={clsx(css.trigger, className)}
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        title={currentWord === undefined ? shown : shown + ' · ' + currentWord}
        onClick={openMenu}
      >
        <span className={css.triggerLabel}>{shown}</span>
        {currentWord !== undefined && <span className={css.triggerEffort}>{currentWord}</span>}
        {fastMode && <span aria-hidden="true">⚡</span>}
        <IconChevronDownOutlineRegular className={clsx(css.chevron, open && css.chevronOpen)} size={12} />
      </button>

      {open && createPortal(
        <div
          ref={menu}
          className={css.menu}
          style={{ left: pos.left, top: pos.top, bottom: pos.bottom }}
          role="menu"
          aria-label="模型选择与推理设置"
        >
          {pane === 'root' && (
            <>
              <button type="button" role="menuitem" className={css.cell} onClick={() => { setPane('model') }}>
                <span className={css.cellLabel}>模型</span>
                <span className={css.cellValue}>{modelLabel === '' ? '未选择' : modelLabel}</span>
                <IconChevronRightOutlineRegular className={css.cellChevron} size={12} />
              </button>
              <button
                type="button"
                role="menuitem"
                className={css.cell}
                disabled={effortOptions.length === 0}
                onClick={() => { setPane('effort') }}
              >
                <span className={css.cellLabel}>思考等级</span>
                <span className={css.cellValue}>{effortOptions.length === 0 ? '不可调节' : effortPhrase(current)}</span>
                <IconChevronRightOutlineRegular className={css.cellChevron} size={12} />
              </button>
              <div className={css.switchCell}>
                <span className={css.cellLabel}>⚡ 请求加速</span>
                <Switch
                  checked={Boolean(fastMode && capability?.fast_mode)}
                  label="请求加速"
                  disabled={onSetGenerationOptions === undefined || capability?.fast_mode !== true}
                  onChange={checked => { void onSetGenerationOptions?.(current, checked) }}
                />
              </div>
              {onOpenSettings !== undefined && (
                <>
                  <div className={css.divider} />
                  <button
                    type="button"
                    role="menuitem"
                    className={css.cell}
                    onClick={() => { setOpen(false); onOpenSettings() }}
                  >
                    <span className={css.cellLabel}>配置模型提供商…</span>
                    <span className={css.cellValue}>{providers.length} 个提供商</span>
                    <IconChevronRightOutlineRegular className={css.cellChevron} size={12} />
                  </button>
                </>
              )}
            </>
          )}

          {pane === 'model' && (
            <>
              <button type="button" role="menuitem" className={clsx(css.cell, css.backCell)} onClick={() => { setPane('root') }}>
                <IconChevronLeftOutlineRegular className={css.cellChevron} size={12} />
                <span className={css.backLabel}>所有模型</span>
              </button>
              <div className={css.divider} />
              <div className={clsx(css.groups, 'scrollable')}>
                {providers.length === 0
                  ? <div className={css.empty}>尚未配置模型提供商</div>
                  : providers.map(provider => (
                    <section key={provider.id} className={css.group}>
                      <div className={css.groupTitle}>{label(provider)}</div>
                      {provider.models.map(model => {
                        const picked = provider.id === activeProvider?.id && model === modelName
                        return (
                          <button
                            key={provider.id + ':' + model}
                            type="button"
                            role="menuitemradio"
                            aria-checked={picked}
                            className={clsx(css.option, picked && css.selected)}
                            onClick={() => { choose(provider, model) }}
                          >
                            <span className={css.optionCopy}>
                              <span className={css.modelName}>{model}</span>
                            </span>
                            <span className={css.check}>{picked ? <IconCheckOutlineRegular size={14} /> : null}</span>
                          </button>
                        )
                      })}
                    </section>
                  ))}
              </div>
            </>
          )}

          {pane === 'effort' && (
            <>
              <button type="button" role="menuitem" className={clsx(css.cell, css.backCell)} onClick={() => { setPane('root') }}>
                <IconChevronLeftOutlineRegular className={css.cellChevron} size={12} />
                <span className={css.backLabel}>思考等级</span>
              </button>
              <div className={css.divider} />
              <div className={clsx(css.groups, 'scrollable')}>
                {effortOptions.map(option => {
                  const picked = current === option.effort
                  return (
                    <button
                      key={option.effort === '' ? 'default' : option.effort}
                      type="button"
                      role="menuitemradio"
                      aria-checked={picked}
                      className={clsx(css.option, picked && css.selected)}
                      onClick={() => {
                        void onSetGenerationOptions?.(option.effort, Boolean(fastMode))
                        setPane('root')
                      }}
                    >
                      <span className={css.optionCopy}>
                        <span className={css.modelName}>{option.label}</span>
                      </span>
                      <span className={css.check}>{picked ? <IconCheckOutlineRegular size={14} /> : null}</span>
                    </button>
                  )
                })}
              </div>
            </>
          )}
        </div>,
        document.body,
      )}
    </>
  )
}
