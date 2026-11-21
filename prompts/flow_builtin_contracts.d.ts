// Declarations only. Write plain JS, without these TypeScript annotations.
// These declarations document runtime-specific signatures and recently added standard APIs.

// Turn-local host timers. Handles are numbers, not Node Timeout objects:
// no ref/unref/refresh, and plain callbacks have undefined this.
declare function setTimeout(callback: (...args: any[]) => unknown, delay?: number | string, ...args: any[]): number;
declare function setInterval(callback: (...args: any[]) => unknown, delay?: number | string, ...args: any[]): number;
declare function clearTimeout(handle?: number | string): void;
declare function clearInterval(handle?: number | string): void;
declare function queueMicrotask(callback: () => unknown): void;

// Host assertion: when condition is falsy, throws message itself, not an Error.
declare function assert(condition: unknown, message?: string): void;

// JSON-compatible values only; collections and iterators are rejected.
declare function structuredClone<T>(value: T): T;

// Holes when the only argument is a number; integer 0..1000000, otherwise RangeError.
declare function Array(length: number): unknown[];
declare function Array<T>(...elements: T[]): T[];

// Modern ECMAScript APIs supported by the resumable VM.
declare global {
  interface ObjectConstructor {
    groupBy<K extends PropertyKey, T>(items: Iterable<T>, callbackFn: (element: T, index: number) => K): Partial<Record<K, T[]>>;
  }
  interface MapConstructor {
    groupBy<K, T>(items: Iterable<T>, callbackFn: (element: T, index: number) => K): Map<K, T[]>;
  }
  interface Array<T> {
    toSpliced(start?: number): T[];
    toSpliced(start: number, deleteCount: number | undefined, ...items: T[]): T[];
    with(index: number, value: T): T[];
  }
  interface ReadonlyArray<T> {
    toSpliced(start?: number): T[];
    toSpliced(start: number, deleteCount: number | undefined, ...items: T[]): T[];
    with(index: number, value: T): T[];
  }
  interface PromiseWithResolvers<T> {
    promise: Promise<T>;
    resolve: (value: T | PromiseLike<T>) => void;
    reject: (reason?: any) => void;
  }
  interface PromiseConstructor {
    withResolvers<T>(): PromiseWithResolvers<T>;
  }
  interface RegExpConstructor {
    escape(str: string): string;
  }
  interface ReadonlySetLike<T> {
    readonly size: number;
    has(value: T): boolean;
    keys(): Iterator<T>;
  }
  interface Set<T> {
    union<U>(other: ReadonlySetLike<U>): Set<T | U>;
    intersection<U>(other: ReadonlySetLike<U>): Set<T & U>;
    difference<U>(other: ReadonlySetLike<U>): Set<T>;
    symmetricDifference<U>(other: ReadonlySetLike<U>): Set<T | U>;
    isSubsetOf(other: ReadonlySetLike<unknown>): boolean;
    isSupersetOf(other: ReadonlySetLike<unknown>): boolean;
    isDisjointFrom(other: ReadonlySetLike<unknown>): boolean;
  }
}

// regex.exec(text) and text.match(nonGlobalRegex) return FlowRegexMatch or null.
// text.matchAll(globalRegex) returns FlowRegexMatch[], an array, not an iterator.
interface FlowRegexMatch extends Array<string | undefined> {
  groups: {[name: string]: string | undefined} | undefined;
  index: number;
  input: string;
}

// Host runtime snapshot & programmable context control: const host = require("host").
// Messages hold compact summaries and saved asset references, so slice/filter/spread stays lightweight.
interface HostContextSnapshot {
  messages: Array<{role: string; content: unknown; name?: string; [k: string]: unknown}>;
  tools: string[];
  libraries: string[];
  prompt_cache_key: string;
  workspace_root: string;
}
interface HostToolsSnapshot {
  tools: string[];
  libraries: Record<string, Record<string, string>>;
  declarations: string;
}
declare module "host" {
  export function context(): HostContextSnapshot;
  export function tools(): HostToolsSnapshot;
}

// Context-as-code requests use one plain-text <run>...</run> block. These
// control-plane calls are available through the injected host namespace and
// still return structured host actions at runtime.
declare namespace host {
  function contracts(args?: unknown): unknown;
  function find_methods(args?: unknown): unknown;
  function guide(args?: unknown): unknown;
  function list_skills(args?: unknown): unknown;
  function read_skill(args: {name: string}): unknown;
  function ask_user(args: unknown): unknown;
}
