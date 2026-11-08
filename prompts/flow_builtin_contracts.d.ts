// Declarations only. Write plain JS, without these TypeScript annotations.
// Existing common APIs remain available; these describe the compatibility additions.
type FlowPropertyKey = string | number | symbol;
declare const Symbol: {
  (description?: unknown): symbol;
  for(key: unknown): symbol; keyFor(value: symbol): string | undefined;
  readonly iterator: unique symbol; readonly asyncIterator: unique symbol;
  readonly hasInstance: unique symbol; readonly toPrimitive: unique symbol; readonly toStringTag: unique symbol;
  readonly prototype: {readonly description: string | undefined; toString(): string; valueOf(): symbol};
};
// Symbol identity, registry, symbol keys and iterator records survive Runtime snapshots.
// new Symbol throws TypeError. Symbol can be used as Reflect.construct's newTarget.
// Symbol.asyncIterator, for-await-of and async generators are available.
// Symbol.species/isConcatSpreadable and regex well-known-symbol hooks are not provided yet.

declare function Object(value?: unknown): object; // new Object(value) also works
interface FlowPropertyDescriptor {
  value?: unknown; writable?: boolean; enumerable?: boolean; configurable?: boolean;
  get?: (() => unknown) | undefined; set?: ((value: any) => void) | undefined;
}
declare namespace Object {
  function create(prototype: object | null, descriptors?: {[key: string | symbol]: FlowPropertyDescriptor}): any;
  function defineProperty<T extends object>(target: T, key: FlowPropertyKey, descriptor: FlowPropertyDescriptor): T;
  function defineProperties<T extends object>(target: T, descriptors: {[key: string | symbol]: FlowPropertyDescriptor}): T;
  function getOwnPropertyDescriptor(target: unknown, key: FlowPropertyKey): FlowPropertyDescriptor | undefined;
  function getOwnPropertyDescriptors(target: unknown): {[key: string | symbol]: FlowPropertyDescriptor};
  function getOwnPropertyNames(target: unknown): string[];
  function getOwnPropertySymbols(target: unknown): symbol[];
  function preventExtensions<T>(target: T): T;
  function seal<T>(target: T): T;
  function isExtensible(target: unknown): boolean;
  function isSealed(target: unknown): boolean;
  function isFrozen(target: unknown): boolean;
  function getPrototypeOf(value: unknown): object | null;
  function setPrototypeOf<T>(value: T, prototype: object | null): T;
}
declare const Function: {readonly prototype: FlowFunction}; // dynamic Function()/new Function() remain unavailable
declare namespace Reflect {
  function get(target: object, key: FlowPropertyKey, receiver?: unknown): any;
  function set(target: object, key: FlowPropertyKey, value: unknown, receiver?: unknown): boolean;
  function has(target: object, key: FlowPropertyKey): boolean;
  function deleteProperty(target: object, key: FlowPropertyKey): boolean;
  function defineProperty(target: object, key: FlowPropertyKey, descriptor: FlowPropertyDescriptor): boolean;
  function getOwnPropertyDescriptor(target: object, key: FlowPropertyKey): FlowPropertyDescriptor | undefined;
  function ownKeys(target: object): (string | symbol)[];
  function getPrototypeOf(target: object): object | null;
  function setPrototypeOf(target: object, prototype: object | null): boolean;
  function isExtensible(target: object): boolean;
  function preventExtensions(target: object): boolean;
  function apply(target: FlowFunction, receiver: unknown, argumentsList: ArrayLike<any>): any;
  function construct(target: {new(...args: any[]): any}, argumentsList: ArrayLike<any>, newTarget?: {new(...args: any[]): any}): any;
}
// Descriptors, accessors and integrity levels survive Runtime snapshots and GC.
// Accessors are ordinary resumable VM functions and keep the original receiver.
// Invalid descriptors/targets throw; Reflect define/delete/set return false for
// a valid operation denied by attributes. Object mutations are strict and throw.
// Ordinary functions: new F(...args), mutable F.prototype, prototype-chain instanceof.
// Arrows/async functions/method shorthand/AI declarations are not constructors.
// Built-in constructor prototypes and methods are shared and non-enumerable.
// Function values use strict/module this. Plain calls: undefined; method calls:
// receiver; arrows: lexical this. Bound receiver/arguments survive snapshots.
interface FlowFunction {
  (...args: any[]): any;
  readonly name: string; readonly length: number;
  prototype?: unknown; // ordinary functions own a prototype; arrows/bound functions do not by default
  call(thisArg: unknown, ...args: any[]): any;
  apply(thisArg: unknown, args?: {readonly length: number; readonly [index: number]: any} | null): any;
  bind(thisArg: unknown, ...prefixArgs: any[]): FlowFunction;
}

// Synchronous iterable protocol. next is cached once for each iteration.
interface FlowIterator<T> {
  next(value?: unknown): {value?: T; done?: boolean};
  return?(value?: unknown): object;
}
interface FlowIterable<T> { [Symbol.iterator](): FlowIterator<T>; }
interface FlowIterableIterator<T> extends FlowIterator<T>, FlowIterable<T> {}
// User iterables may return ordinary objects with next; built-in iterators are iterable.
// Array/String/Map/Set, for-of, spread, destructuring, Array.from, collection constructors,
// Object.fromEntries and Promise combinators use Symbol.iterator. Abrupt consumer exits
// call return with the iterator receiver; async-only objects are not synchronous iterables.
interface FlowMap<K,V> extends FlowIterable<[K,V]> {
  readonly size: number;
  get(key: K): V | undefined; set(key: K, value: V): FlowMap<K,V>;
  has(key: K): boolean; delete(key: K): boolean; clear(): void;
  keys(): FlowIterableIterator<K>; values(): FlowIterableIterator<V>; entries(): FlowIterableIterator<[K,V]>;
  forEach(callback: (value: V, key: K, map: FlowMap<K,V>) => void, thisArg?: unknown): void;
}
declare const Map: {new<K=unknown,V=unknown>(entries?: FlowIterable<[K,V]> | null): FlowMap<K,V>};
interface FlowSet<T> extends FlowIterable<T> {
  readonly size: number; add(value: T): FlowSet<T>; has(value: T): boolean;
  delete(value: T): boolean; clear(): void;
  keys(): FlowIterableIterator<T>; values(): FlowIterableIterator<T>; entries(): FlowIterableIterator<[T,T]>;
  forEach(callback: (value: T, key: T, set: FlowSet<T>) => void, thisArg?: unknown): void;
}
declare const Set: {new<T=unknown>(values?: FlowIterable<T> | null): FlowSet<T>};
interface FlowWeakMap<K extends object,V> {
  get(key: K): V | undefined; set(key: K, value: V): FlowWeakMap<K,V>;
  has(key: K): boolean; delete(key: K): boolean;
}
declare const WeakMap: {new<K extends object=object,V=unknown>(entries?: FlowIterable<[K,V]> | null): FlowWeakMap<K,V>};
interface FlowWeakSet<T extends object> {
  add(value: T): FlowWeakSet<T>; has(value: T): boolean; delete(value: T): boolean;
}
declare const WeakSet: {new<T extends object=object>(values?: FlowIterable<T> | null): FlowWeakSet<T>};
// Weak collections retain object keys strongly, expose no GC behavior or enumeration.
// Runtime snapshots retain entries/cursors. Tool/AI JSON boundaries serialize only
// enumerable properties (an unadorned collection is {}). Pass Array.from(...) for data.
// forEach supports thisArg; ordinary callbacks are strict, arrows capture lexical this.
// structuredClone remains JSON-only and rejects collections/iterators, including nested ones.
declare function Array<T>(...elements: T[]): T[];
declare function Array(length: number): unknown[]; // holes; integer 0..1000000, otherwise RangeError
// new Array has the same overloads; fill(value,start?,end?) mutates and returns it.
declare namespace String {
  function raw(template: {raw: string[]}, ...substitutions: unknown[]): string; // tagged-template syntax only
  function fromCharCode(...units: number[]): string; // ToUint16; preserves surrogate pairs/code units
  function fromCodePoint(...points: number[]): string; // integer 0..0x10ffff, otherwise RangeError
}
declare namespace Object {
  function hasOwn(object: unknown, key: FlowPropertyKey): boolean; // null/undefined throw TypeError
  function freeze<T>(object: T): T; // shallow, same identity; strict mutation throws TypeError
}
declare function encodeURIComponent(value: unknown): string; // unpaired surrogate throws URIError
declare function decodeURIComponent(value: unknown): string; // malformed percent/UTF-8 throws URIError
declare function structuredClone<T>(value: T): T; // JSON-compatible values; deep independent copy
// Turn-local host timers. Numeric handles, not Node Timeout objects.
declare function setTimeout(callback: (...args: any[]) => unknown, delay?: number | string, ...args: any[]): number;
declare function setInterval(callback: (...args: any[]) => unknown, delay?: number | string, ...args: any[]): number;
declare function clearTimeout(handle?: number | string): void;
declare function clearInterval(handle?: number | string): void;
declare function queueMicrotask(callback: () => unknown): void;
// Promise/queueMicrotask FIFO drains before timers. Uncleared timers keep the
// program alive; callback throws are uncaught. Snapshots preserve due times,
// callbacks and arguments. Overdue intervals fire once; stop/end cancels them.
// No ref/unref/refresh or Node Timeout this object. Plain callback this is undefined;
// bound callbacks/arrows preserve their receiver. No cross-turn scheduling.
interface FlowRegexMatch extends Array<string | undefined> {
  groups: {[name: string]: string | undefined} | undefined;
  index: number;
  input: string;
}
// regex.exec(text), text.match(nonGlobalRegex): FlowRegexMatch | null
// text.matchAll(globalRegex): FlowRegexMatch[]; global match: string[] | null (JS semantics).
interface FlowDate {
  getTime(): number; valueOf(): number;
  toISOString(): string; // RangeError for invalid date
  toJSON(): string | null; toString(): string;
  getFullYear(): number; getMonth(): number; getDate(): number; getDay(): number;
  getHours(): number; getMinutes(): number; getSeconds(): number; getMilliseconds(): number;
  getUTCFullYear(): number; getUTCMonth(): number; getUTCDate(): number; getUTCDay(): number;
  getUTCHours(): number; getUTCMinutes(): number; getUTCSeconds(): number; getUTCMilliseconds(): number;
}
declare const Date: {
  new(): FlowDate;
  new(value: number | string | FlowDate): FlowDate;
  new(year: number, month: number, date?: number, hours?: number, minutes?: number, seconds?: number, milliseconds?: number): FlowDate;
  (): string;
  now(): number;
  parse(iso: string): number;
  UTC(year: number, month?: number, date?: number, hours?: number, minutes?: number, seconds?: number, milliseconds?: number): number;
};
// Local methods/construction use the host timezone. Date values survive snapshots.
// +date, date-date, < > <= >= and instanceof Date work; JSON.stringify uses toJSON.
// No setters or toLocale*: locale data is absent; use toISOString or component getters.

// Class syntax: class declarations/expressions, constructor, extends,
// instance/static methods, computed names, getters/setters, super(),
// super.method/get/set/update and new.target are supported in strict mode.
// Native subclasses retain their internal slots (Error, Array, collections,
// Date, Promise, Number/String/Boolean). Their species algorithms are not provided.
// Methods are non-enumerable, non-constructable and carry a durable home object.
// Arrows capture this/new.target/super; ordinary calls observe new.target === undefined.
// Derived this is initialized once by super(). Constructor/home-object state
// survives GC, host suspension and Runtime snapshots; JSON carries only data.
// Public/private fields, static blocks, Function/eval and async iteration
// are not supported. Class name assignment throws TypeError at runtime.

interface Number { toExponential(fractionDigits?: number): string; }
// toExponential: receiver must be Number/boxed Number; digits 0..100,
// shortest notation when omitted; argument coercion can suspend/resume.
// Anonymous function/arrow/class names are inferred in bindings, assignments,
// destructuring/parameter defaults and object property values; explicit names
// and class static name properties are retained. UTF-16 names stay lossless.
// Promise callbacks use the normal VM call path. Local callbacks do not cross
// a JSON boundary; real host calls still require their documented JSON inputs.
// Native callback failures report the registration source location.

// Synchronous generators are standard syntax: function* / yield / yield*.
// Parameters initialize at call time; the body starts on first next().
// Suspended frames and pending catch/finally completion survive Runtime snapshots.
interface FlowGeneratorResult<T = unknown> { value: T | undefined; done: boolean; }
interface FlowGenerator<Y = unknown, R = unknown, N = unknown> {
  next(value?: N): FlowGeneratorResult<Y | R>;
  return(value?: R): FlowGeneratorResult<Y | R>;
  throw(error?: unknown): FlowGeneratorResult<Y | R>;
  [Symbol.iterator](): FlowGenerator<Y, R, N>;
}
// No dynamic GeneratorFunction/AsyncGeneratorFunction or structuredClone(generator).


// Async syntax: async function* / async generator methods / for await...of.
// Queued requests and awaited values survive Runtime snapshots, including
// host calls inside next/return, finally, Promise constructor/then getters.
interface FlowAsyncGenerator<Y = unknown, R = unknown, N = unknown> {
  next(value?: N): Promise<FlowGeneratorResult<Y | R>>;
  return(value?: R | PromiseLike<R>): Promise<FlowGeneratorResult<Y | R>>;
  throw(error?: unknown): Promise<FlowGeneratorResult<Y | R>>;
  [Symbol.asyncIterator](): FlowAsyncGenerator<Y, R, N>;
}
// for-await adapts synchronous iterators and awaits their values. Actual
// async iterator values are retained; break/return/throw await iterator cleanup.
// Await assimilates thenables; general Promise.resolve/executor assimilation
// is still incomplete. Dynamic AsyncGeneratorFunction remains unavailable.
