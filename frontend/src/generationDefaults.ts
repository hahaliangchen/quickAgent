import type { SaveSettingsInput, SettingsData, TaskStatus } from './api.ts'

/** A model choice plus the levels it implies, remembered for the next chat. */
export interface GenerationDefaults {
  readonly provider?: string | null | undefined
  readonly model?: string | null | undefined
  readonly reasoning_effort: string
  readonly fast_mode: boolean
}

/**
 * A chat that is still turning cannot take a new model or a new level: the run
 * in flight already resolved its own, and the server answers such an update with
 * "task is currently running". Asking anyway is what used to abort the save that
 * carries the pick to the next chat.
 * @param status - The chat's status, when it is known.
 * @returns Whether the chat's own row must be left alone until it settles.
 */
export function generationLocked(status: TaskStatus | undefined): boolean {
  return status === 'running' || status === 'cancelling'
}

/** A value that names something, else the fallback: an empty pick is not a choice. */
function chosen(value: string | null | undefined, fallback: string | null): string | null {
  return typeof value === 'string' && value !== '' ? value : fallback
}

/**
 * The settings write that makes a pick the model the next chat opens with.
 * The orchestrator pair is spelled out even when the pick only changes a level:
 * it is the field a new chat takes its model from, and a write that leaves it
 * out clears the default instead of keeping it.
 * @param settings - The settings document the write rebases onto.
 * @param defaults - The provider, model and levels to remember.
 * @returns The full settings payload, ready to PUT.
 */
export function defaultsPayload(settings: SettingsData, defaults: GenerationDefaults): SaveSettingsInput {
  return {
    revision: settings.revision,
    providers: settings.providers.map(provider => ({
      id: provider.id,
      display_name: provider.display_name ?? null,
      url: provider.url,
      api_type: provider.api_type,
      models: provider.models,
      model_capabilities: provider.model_capabilities,
    })),
    orchestrator_provider: chosen(defaults.provider, settings.orchestrator_provider),
    orchestrator_model: chosen(defaults.model, settings.orchestrator_model),
    reasoning_effort: defaults.reasoning_effort,
    fast_mode: defaults.fast_mode,
  }
}
