/**
 * Detachable Experimental Feature: Blueprint Panoramic Observer.
 *
 * To completely remove this experimental feature:
 * 1. Delete `frontend/src/experimental/blueprintObserver/` and `frontend/tests/blueprint-observer.test.mjs`.
 * 2. Remove the single `<BlueprintObserverHost ... />` import & mount in `frontend/src/App.tsx`.
 */

export const BLUEPRINT_OBSERVER_ENABLED = true
export { BlueprintObserverHost, type BlueprintObserverHostProps } from './BlueprintObserverHost.tsx'
