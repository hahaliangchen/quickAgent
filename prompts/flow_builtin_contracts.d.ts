// Declarations only. Write plain JS, without these TypeScript annotations.
// Standard built-ins follow the ECMAScript standard and are not repeated here.
// These are the signatures that differ from it or from Node.

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
