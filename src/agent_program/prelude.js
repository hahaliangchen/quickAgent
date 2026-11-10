// Internal resumable implementations. The receiver and callback are ordinary
// VM values so calls and captured environments survive host suspension.
function $$collectionForEach(collection, callback, thisArg) {
  const iterator = $$collectionEntries(collection);
  while (true) {
    const step = iterator.next();
    if (step.done) return;
    callback.call(thisArg, step.value[1], step.value[0], collection);
  }
}
function $$arrayFrom(items,mapper,thisArg){
  if(mapper!==undefined&&typeof mapper!=='function')throw new TypeError('Array.from mapper must be callable');
  $$requireCoercible(items);const method=items[Symbol.iterator],out=[];
  if(method===undefined||method===null){const n=$$arrayLikeLength(items);for(let i=0;i<n;i++){const v=items[i];out.push(mapper===undefined?v:$$invokeWithReceiver(mapper,thisArg,[v,i]));}return out;}
  const record=$$getIteratorMethod(items,method);let i=0;
  try{while(true){const step=$$iteratorStep(record);if(step.done)return out;const v=mapper===undefined?step.value:$$invokeWithReceiver(mapper,thisArg,[step.value,i]);out.push(v);i++;}}
  catch(error){$$iteratorCloseThrow(record,error);}
}
function $$map(a, f, thisArg) {
  const n = a.length; const out = []; out.length = n;
  for (let i = 0; i < n; i++) if ($$has(a, i)) out[i] = f.call(thisArg, a[i], i, a);
  return out;
}
function $$filter(a, f, thisArg) {
  const n = a.length; const out = [];
  for (let i = 0; i < n; i++) if ($$has(a, i)) { const value = a[i]; if (f.call(thisArg, value, i, a)) out.push(value); }
  return out;
}
function $$forEach(a, f, thisArg) { const n = a.length; for (let i = 0; i < n; i++) if ($$has(a, i)) f.call(thisArg, a[i], i, a); }
function $$some(a, f, thisArg) { const n = a.length; for (let i = 0; i < n; i++) if ($$has(a, i) && f.call(thisArg, a[i], i, a)) return true; return false; }
function $$every(a, f, thisArg) { const n = a.length; for (let i = 0; i < n; i++) if ($$has(a, i) && !f.call(thisArg, a[i], i, a)) return false; return true; }
function $$find(a, f, thisArg) { const n = a.length; for (let i = 0; i < n; i++) { const value = a[i]; if (f.call(thisArg, value, i, a)) return value; } }
function $$findIndex(a, f, thisArg) { const n = a.length; for (let i = 0; i < n; i++) if (f.call(thisArg, a[i], i, a)) return i; return -1; }
function $$findLast(a, f, thisArg) { for (let i = a.length - 1; i >= 0; i--) { const value = a[i]; if (f.call(thisArg, value, i, a)) return value; } }
function $$findLastIndex(a, f, thisArg) { for (let i = a.length - 1; i >= 0; i--) if (f.call(thisArg, a[i], i, a)) return i; return -1; }
function $$flatMap(a, f, thisArg) { return $$map(a, f, thisArg).flat(1); }
function $$reduce(a, f, initial, supplied) {
  const n = a.length; let i = 0; let result = initial;
  if (!supplied) { while (i < n && !$$has(a, i)) i++; if (i >= n) throw new TypeError('Reduce of empty array with no initial value'); result = a[i]; i++; }
  for (; i < n; i++) if ($$has(a, i)) result = f(result, a[i], i, a);
  return result;
}
function $$reduceRight(a, f, initial, supplied) {
  let i = a.length - 1; let result = initial;
  if (!supplied) { while (i >= 0 && !$$has(a, i)) i--; if (i < 0) throw new TypeError('Reduce of empty array with no initial value'); result = a[i]; i--; }
  for (; i >= 0; i--) if ($$has(a, i)) result = f(result, a[i], i, a);
  return result;
}
function $$sortCore(a, compare, copy) {
  if(compare===undefined)compare=function(left,right){left=String(left);right=String(right);return left<right?-1:left>right?1:0;};
  else if(typeof compare!=='function')throw new TypeError('Sort comparator must be callable or undefined');
  const n = a.length; let values = []; let missing = 0;
  for (let i = 0; i < n; i++) {
    if (copy || $$has(a, i)) {
      const value = a[i];
      if (value === undefined) missing++; else values.push(value);
    }
  }
  const count = values.length; let temp = [];
  for (let width = 1; width < count; width *= 2) {
    for (let i = 0; i < count; i += width * 2) {
      let left = i; let right = Math.min(i + width, count);
      const middle = right; const end = Math.min(i + width * 2, count);
      for (let k = i; k < end; k++) {
        if (left >= middle) { temp[k] = values[right]; right++; }
        else if (right >= end) { temp[k] = values[left]; left++; }
        else if (+compare(values[left], values[right]) > 0) { temp[k] = values[right]; right++; }
        else { temp[k] = values[left]; left++; }
      }
    }
    const swap = values; values = temp; temp = swap;
  }
  for (let i = 0; i < missing; i++) values.push(undefined);
  if (copy) return values;
  return $$sortWriteBack(a, values);
}
function $$sort(a, compare) { return $$sortCore(a, compare, false); }
function $$toSorted(a, compare) { return $$sortCore(a, compare, true); }
function $$replace(text, pattern, replacement, all) {
  const matches = $$replacementMatches(text, pattern, all);
  let out = ''; let previous = 0;
  for (let i = 0; i < matches.length; i++) {
    const match = matches[i];
    out += text.slice(previous, match.start);
    match.args.push(match.start, text);
    if (match.hasGroups) match.args.push(match.groups);
    out = $$textLimit(out + String($$apply(replacement, match.args)));
    previous = match.end;
  }
  return $$textLimit(out + text.slice(previous));
}

// Promise combinators are ordinary resumable JavaScript functions.
function $$promiseAll(values){return new Promise((resolve,reject)=>{let remaining=1,index=0;const result=[];for(const value of values){const i=index++;remaining++;Promise.resolve(value).then(v=>{result[i]=v;remaining--;if(remaining===0)resolve(result);},reject);}remaining--;if(remaining===0)resolve(result);});}
function $$promiseAllSettled(values){return new Promise(resolve=>{let remaining=1,index=0;const result=[];for(const value of values){const i=index++;remaining++;Promise.resolve(value).then(v=>{result[i]={status:'fulfilled',value:v};remaining--;if(remaining===0)resolve(result);},reason=>{result[i]={status:'rejected',reason};remaining--;if(remaining===0)resolve(result);});}remaining--;if(remaining===0)resolve(result);});}
function $$promiseRace(values){return new Promise((resolve,reject)=>{for(const value of values)Promise.resolve(value).then(resolve,reject);});}
function $$promiseAny(values){return new Promise((resolve,reject)=>{let remaining=1,index=0;const errors=[];for(const value of values){const i=index++;remaining++;Promise.resolve(value).then(resolve,reason=>{errors[i]=reason;remaining--;if(remaining===0){const e=new Error('All promises were rejected');e.name='AggregateError';e.errors=errors;reject(e);}});}remaining--;if(remaining===0){const e=new Error('All promises were rejected');e.name='AggregateError';e.errors=errors;reject(e);}});}
function $$runCallback(callback, args) { $$apply(callback, args); }
function $$arrayToString(array) { const join=array.join; if(typeof join==='function') return join.call(array); return Object.prototype.toString.call(array); }
function $$promiseReaction(callback, value) { return callback(value); }
function $$promiseFinally(promise, callback) {
  if (typeof callback !== 'function') return promise.then(callback, callback);
  return promise.then(value => Promise.resolve(callback()).then(() => value),
    reason => Promise.resolve(callback()).then(() => { throw reason; }));
}

function $$toDescriptor(input) {
  if (!$$isObject(input)) throw new TypeError('Property descriptor must be an object');
  const out = $$createObject(null);
  if ('enumerable' in input) out.enumerable = !!input.enumerable;
  if ('configurable' in input) out.configurable = !!input.configurable;
  if ('value' in input) out.value = input.value;
  if ('writable' in input) out.writable = !!input.writable;
  if ('get' in input) { const g=input.get; if(g!==undefined&&typeof g!=='function') throw new TypeError('Getter must be callable or undefined'); out.get=g; }
  if ('set' in input) { const s=input.set; if(s!==undefined&&typeof s!=='function') throw new TypeError('Setter must be callable or undefined'); out.set=s; }
  if (('get' in out || 'set' in out) && ('value' in out || 'writable' in out)) throw new TypeError('Invalid mixed property descriptor');
  return out;
}
function $$defineProperty(target, key, descriptor) {
  if (!$$isObject(target)) throw new TypeError('Object.defineProperty target must be an object');
  key=$$toPropertyKey(key); const d=$$toDescriptor(descriptor);
  if (!$$defineOwn(target,key,d)) throw new TypeError('Cannot redefine property '+String(key));
  return target;
}
function $$reflectDefineProperty(target,key,descriptor) {
  if (!$$isObject(target)) throw new TypeError('Reflect target must be an object');
  key=$$toPropertyKey(key); const d=$$toDescriptor(descriptor); return $$defineOwn(target,key,d);
}
function $$defineProperties(target, descriptors) {
  if (!$$isObject(target)) throw new TypeError('Object.defineProperties target must be an object');
  $$requireCoercible(descriptors);const source=Object(descriptors),keys=Reflect.ownKeys(source),pending=[];
  for(let i=0;i<keys.length;i++){const key=keys[i];if(Object.prototype.propertyIsEnumerable.call(source,key)) pending.push([key,$$toDescriptor(source[key])]);}
  for(let i=0;i<pending.length;i++) if(!$$defineOwn(target,pending[i][0],pending[i][1])) throw new TypeError('Cannot redefine property '+pending[i][0]);
  return target;
}
function $$objectCreate(proto,descriptors){const result=$$createObject(proto);if(descriptors!==undefined) $$defineProperties(result,descriptors);return result;}
function $$objectValues(input){const source=Object(input),keys=$$keys(input),out=[];for(let i=0;i<keys.length;i++){const k=keys[i];if(Object.prototype.propertyIsEnumerable.call(source,k))out.push(source[k]);}return out;}
function $$objectEntries(input){const source=Object(input),keys=$$keys(input),out=[];for(let i=0;i<keys.length;i++){const k=keys[i];if(Object.prototype.propertyIsEnumerable.call(source,k))out.push([k,source[k]]);}return out;}
function $$objectAssign(target,...sources){$$requireCoercible(target);target=Object(target);for(let n=0;n<sources.length;n++){const source=sources[n];if(source===null||source===undefined)continue;const keys=Reflect.ownKeys(Object(source));for(let i=0;i<keys.length;i++){const k=keys[i];if(Object.prototype.propertyIsEnumerable.call(source,k))target[k]=source[k];}}return target;}
function $$spreadObject(target,source){if(source===null||source===undefined)return target;const keys=Reflect.ownKeys(Object(source));for(let i=0;i<keys.length;i++){const k=keys[i];if(Object.prototype.propertyIsEnumerable.call(source,k))$$createData(target,k,source[k]);}return target;}
function $$objectDescriptors(input){$$requireCoercible(input);const source=Object(input),keys=Reflect.ownKeys(source),out={};for(let i=0;i<keys.length;i++){const key=keys[i],d=Object.getOwnPropertyDescriptor(source,key);if(d!==undefined)$$createData(out,key,d);}return out;}
function $$reflectGet(...args){const target=args[0];if(!$$isObject(target))throw new TypeError('Reflect target must be an object');return $$getProperty(target,$$toPropertyKey(args[1]),args.length>2?args[2]:target);}
function $$reflectSet(...args){const target=args[0];if(!$$isObject(target))throw new TypeError('Reflect target must be an object');const receiver=args.length>3?args[3]:target,plan=$$prepareSet(target,$$toPropertyKey(args[1]),args[2],receiver);if(!plan[0])return false;if(plan[1]!==undefined)plan[1].call(receiver,args[2]);return true;}
function $$strictSet(target,key,value){const plan=$$prepareSet(target,key,value,target);if(!plan[0])throw new TypeError('Cannot assign property '+String(key));if(plan[1]!==undefined)plan[1].call(target,value);}
function $$argumentList(list){if(!$$isObject(list))throw new TypeError('Argument list must be an object');let n=Number(list.length);n=Number.isNaN(n)||n<0?0:Math.trunc(n);if(n>1000000)throw new RangeError('Argument list exceeds runtime limit');const out=[];for(let i=0;i<n;i++)out.push(list[i]);return out;}
function $$reflectApply(target,receiver,list){if(typeof target!=='function')throw new TypeError('Reflect.apply target must be callable');return $$invokeWithReceiver(target,receiver,$$argumentList(list));}
function $$functionApply(target,receiver,list){if(typeof target!=='function')throw new TypeError('Function.apply receiver must be callable');return $$invokeWithReceiver(target,receiver,list===null||list===undefined?[]:$$argumentList(list));}
function $$reflectConstruct(...args){const target=args[0],list=args[1],newTarget=args.length>2?args[2]:target;if(!$$isConstructor(target)||!$$isConstructor(newTarget))throw new TypeError('Reflect.construct target must be a constructor');let effective=newTarget,callee=target;while($$boundTarget(callee)!==undefined){const bound=$$boundTarget(callee);if(effective===callee)effective=bound;callee=bound;}const values=$$argumentList(list);return $$constructWithTarget(target,values,effective);}
function $$jsonProject(value,key,stack,keys){
  if($$isObject(value)){const toJSON=value.toJSON;if(typeof toJSON==='function')value=toJSON.call(value,key);}
  value=$$jsonUnbox(value);
  if(!$$isObject(value)||typeof value==='function')return value;
  if(stack.includes(value))throw new TypeError('Converting circular structure to JSON');
  stack.push(value);let out;
  if(Array.isArray(value)){out=[];const n=value.length;for(let i=0;i<n;i++)out.push($$jsonProject(value[i],String(i),stack,keys));}
  else {out={};const names=keys===undefined?$$keys(value):keys;for(let i=0;i<names.length;i++){const k=names[i];$$createData(out,k,$$jsonProject(value[k],k,stack,keys));}}
  stack.pop();return out;
}
function $$jsonStringify(value,replacer,space){
  if(typeof replacer==='function')throw new TypeError('JSON.stringify replacer functions are not available yet');
  let keys;
  if(Array.isArray(replacer)){keys=[];for(let i=0;i<replacer.length;i++){const k=$$jsonUnbox(replacer[i]);if(typeof k==='string'||typeof k==='number'){const s=String(k);if(!keys.includes(s))keys.push(s);}}}
  return $$rawStringify($$jsonProject(value,'',[],keys),replacer,space);
}
function $$arrayPush(a,...items){const n=a.length;for(let i=0;i<items.length;i++)a[n+i]=items[i];a.length=n+items.length;return a.length;}
function $$arrayPop(a){const n=a.length;if(n===0){a.length=0;return;}const value=a[n-1];delete a[n-1];a.length=n-1;return value;}
function $$relativeIndex(value,n,fallback){if(value===undefined)return fallback;let i=Number(value);i=Number.isNaN(i)?0:Math.trunc(i);return i<0?Math.max(n+i,0):Math.min(i,n);}
function $$arrayFill(a,value,start,end){const n=a.length,s=$$relativeIndex(start,n,0),e=$$relativeIndex(end,n,n);for(let i=s;i<e;i++)a[i]=value;return a;}
function $$arrayReverse(a){const n=a.length;for(let lo=0;lo<Math.floor(n/2);lo++){const hi=n-lo-1,lh=lo in a,lv=lh?a[lo]:undefined,rh=hi in a,rv=rh?a[hi]:undefined;if(lh&&rh){a[lo]=rv;a[hi]=lv;}else if(rh){a[lo]=rv;delete a[hi];}else if(lh){delete a[lo];a[hi]=lv;}}return a;}
function $$arrayShift(a){const n=a.length;if(n===0){a.length=0;return;}const first=a[0];for(let k=1;k<n;k++){if(k in a)a[k-1]=a[k];else delete a[k-1];}delete a[n-1];a.length=n-1;return first;}
function $$arrayUnshift(a,...items){const n=a.length,m=items.length;if(m>0){for(let k=n;k>0;k--){if(k-1 in a)a[k+m-1]=a[k-1];else delete a[k+m-1];}for(let k=0;k<m;k++)a[k]=items[k];}a.length=n+m;return n+m;}
function $$arraySplice(a,...args){const n=a.length,s=$$relativeIndex(args[0],n,0),count=args.length===0?0:args.length===1?n-s:Math.min(Math.max(Math.trunc(Number(args[1]))||0,0),n-s),m=Math.max(args.length-2,0),out=[];out.length=count;for(let i=0;i<count;i++)if(s+i in a)out[i]=a[s+i];if(m<count){for(let k=s;k<n-count;k++){if(k+count in a)a[k+m]=a[k+count];else delete a[k+m];}for(let k=n;k>n-count+m;k--)delete a[k-1];}else if(m>count){for(let k=n-count;k>s;k--){if(k+count-1 in a)a[k+m-1]=a[k+count-1];else delete a[k+m-1];}}for(let k=0;k<m;k++)a[s+k]=args[k+2];a.length=n-count+m;return out;}
function $$arrayCopyWithin(a,target,start,end){const n=a.length;let to=$$relativeIndex(target,n,0),from=$$relativeIndex(start,n,0),count=Math.min($$relativeIndex(end,n,n)-from,n-to),step=1;if(from<to&&to<from+count){step=-1;from+=count-1;to+=count-1;}for(let i=0;i<count;i++){if(from in a)a[to]=a[from];else delete a[to];from+=step;to+=step;}return a;}
function $$sortApply(a,values){const n=a.length;for(let i=0;i<values.length;i++)a[i]=values[i];for(let i=values.length;i<n;i++)delete a[i];return a;}

function $$toPropertyKey(value){if($$isObject(value)){const exotic=value[Symbol.toPrimitive];if(exotic!==undefined&&exotic!==null){if(typeof exotic!=='function')throw new TypeError('Symbol.toPrimitive must be callable');value=$$invokeWithReceiver(exotic,value,['string']);if($$isObject(value))throw new TypeError('Cannot convert object to property key');}else{const original=value,string=value.toString;let done=false;if(typeof string==='function'){const p=$$invokeWithReceiver(string,original,[]);if(!$$isObject(p)){value=p;done=true;}}if(!done){const number=original.valueOf;if(typeof number==='function'){const p=$$invokeWithReceiver(number,original,[]);if(!$$isObject(p)){value=p;done=true;}}}if(!done)throw new TypeError('Cannot convert object to property key');}}return typeof value==='symbol'?value:String(value);}
function $$ordinaryHasInstance(value,ctor){if(typeof ctor!=='function')return false;const target=$$boundTarget(ctor);if(target!==undefined)return $$instanceOf(value,target);if(!$$isObject(value))return false;const proto=ctor.prototype;if(!$$isObject(proto))throw new TypeError('Function has non-object prototype in instanceof');return $$hasPrototype(value,proto);}
function $$instanceOf(value,ctor){if(!$$isObject(ctor))throw new TypeError('Right-hand side of instanceof is not an object');const method=ctor[Symbol.hasInstance];if(method!==undefined&&method!==null){if(typeof method!=='function')throw new TypeError('Symbol.hasInstance must be callable');return !!$$invokeWithReceiver(method,ctor,[value]);}if(typeof ctor!=='function')throw new TypeError('Right-hand side of instanceof is not callable');return $$ordinaryHasInstance(value,ctor);}
function $$objectToString(value){const base=$$defaultTag(value);if(value===undefined||value===null)return base;const tag=value[Symbol.toStringTag];return typeof tag==='string'?'[object '+tag+']':base;}

function $$iteratorNext(iterator){const p=$$iteratorPlan(iterator);if(p.done)return {done:true,value:undefined};let value=p.indirect?p.source[p.index]:p.value;if(p.kind==='keys')value=p.index;else if(p.kind==='entries'&&p.indirect)value=[p.index,value];return {done:false,value};}
function $$iteratorRead(record){const step=$$iteratorStep(record);return step.done?undefined:step.value;}
function $$arrayLikeLength(value){let n=Number(value.length);n=Number.isNaN(n)||n<0?0:Math.trunc(n);if(n>1000000)throw new RangeError('Array-like exceeds runtime limit');return n;}

function $$functionBind(target,...args){if(typeof target!=='function')throw new TypeError('Bind target must be callable');let length=Object.hasOwn(target,'length')?target.length:0;const name=target.name,prefix=args.slice(1);length=typeof length==='number'&&!Number.isNaN(length)?Math.max(Math.trunc(length)-prefix.length,0):0;return $$makeBound(target,args[0],prefix,'bound '+(typeof name==='string'?name:''),length);}

function $$reflectHas(target,key){if(!$$isObject(target))throw new TypeError('Reflect target must be an object');return $$keyOperation('Reflect.has',target,$$toPropertyKey(key));}
function $$reflectDelete(target,key){if(!$$isObject(target))throw new TypeError('Reflect target must be an object');return $$keyOperation('Reflect.deleteProperty',target,$$toPropertyKey(key));}
function $$reflectDescriptor(target,key){if(!$$isObject(target))throw new TypeError('Reflect target must be an object');return $$keyOperation('Reflect.getOwnPropertyDescriptor',target,$$toPropertyKey(key));}
function $$objectDescriptor(target,key){$$requireCoercible(target);target=Object(target);return $$keyOperation('Object.getOwnPropertyDescriptor',target,$$toPropertyKey(key));}

function $$getIteratorMethod(value,method){if(typeof method!=='function')throw new TypeError('Symbol.iterator must be callable');const iterator=$$invokeWithReceiver(method,value,[]);if(!$$isObject(iterator))throw new TypeError('Symbol.iterator must return an object');return {iterator,next:iterator.next,done:false};}
function $$getIterator(value){$$requireCoercible(value);return $$getIteratorMethod(value,value[Symbol.iterator]);}
function $$iteratorStep(record,readValue=true){if(record.done)return {done:true,value:undefined};try{const step=$$invokeWithReceiver(record.next,record.iterator,[]);if(!$$isObject(step))throw new TypeError('Iterator result must be an object');if(step.done){record.done=true;return {done:true,value:undefined};}return {done:false,value:readValue?step.value:undefined};}catch(error){record.done=true;throw error;}}
function $$iteratorCloseNormal(record){if(record.done)return;record.done=true;const method=record.iterator.return;if(method===undefined||method===null)return;if(typeof method!=='function')throw new TypeError('Iterator return must be callable');const result=$$invokeWithReceiver(method,record.iterator,[]);if(!$$isObject(result))throw new TypeError('Iterator return result must be an object');}
function $$iteratorCloseThrow(record,error){try{$$iteratorCloseNormal(record);}catch(ignored){}throw error;}
function $$iteratorElide(record){$$iteratorStep(record,false);}
function $$iteratorRest(record){const out=[];while(true){const step=$$iteratorStep(record);if(step.done)return out;out.push(step.value);}}
function $$iterableToArray(value){const record=$$getIterator(value);try{return $$iteratorRest(record);}catch(error){$$iteratorCloseThrow(record,error);}}
function $$constructCollection(name,source,prototype){const out=$$emptyCollection(name);if($$isObject(prototype))Object.setPrototypeOf(out,prototype);if(source===null||source===undefined)return out;const map=name==='Map'||name==='WeakMap',adder=map?out.set:out.add;if(typeof adder!=='function')throw new TypeError('Collection adder must be callable');for(const item of source){if(map){if(!$$isObject(item))throw new TypeError('Map entry must be an object');const key=item[0],value=item[1];$$invokeWithReceiver(adder,out,[key,value]);}else{$$invokeWithReceiver(adder,out,[item]);}}return out;}
function $$constructMap(source){return $$constructCollection('Map',source);}
function $$constructSet(source){return $$constructCollection('Set',source);}
function $$constructWeakMap(source){return $$constructCollection('WeakMap',source);}
function $$constructWeakSet(source){return $$constructCollection('WeakSet',source);}
function $$fromEntries(source){$$requireCoercible(source);const out={};for(const entry of source){if(!$$isObject(entry))throw new TypeError('Iterator entry must be an object');const key=entry[0],value=entry[1];$$createData(out,$$toPropertyKey(key),value);}return out;}

function $$objectRest(value,excluded){$$requireCoercible(value);const out={},source=Object(value),keys=Reflect.ownKeys(source);for(let i=0;i<keys.length;i++){const key=keys[i];if(!excluded.includes(key)&&Object.prototype.propertyIsEnumerable.call(source,key))$$createData(out,key,source[key]);}return out;}

function $$toPrimitiveHint(value,hint){if(!$$isObject(value))return value;const exotic=value[Symbol.toPrimitive];if(exotic!==undefined&&exotic!==null){if(typeof exotic!=='function')throw new TypeError('Symbol.toPrimitive must be callable');const p=$$invokeWithReceiver(exotic,value,[hint]);if($$isObject(p))throw new TypeError('Symbol.toPrimitive must return a primitive');return p;}if(hint==='default')hint=$$isDate(value)?'string':'number';const first=hint==='string'?'toString':'valueOf',second=hint==='string'?'valueOf':'toString';let method=value[first];if(typeof method==='function'){const p=$$invokeWithReceiver(method,value,[]);if(!$$isObject(p))return p;}method=value[second];if(typeof method==='function'){const p=$$invokeWithReceiver(method,value,[]);if(!$$isObject(p))return p;}throw new TypeError('Cannot convert object to primitive');}
function $$coercedEqual(a,b,invert){if($$isObject(a))a=$$toPrimitiveHint(a,'default');if($$isObject(b))b=$$toPrimitiveHint(b,'default');const equal=$$rawLooseEqual(a,b);return invert?!equal:equal;}
function $$stringConvert(value){const p=$$toPrimitiveHint(value,'string');if(typeof p==='symbol')throw new TypeError('Cannot convert a Symbol value to a string');return $$rawString(p);}
function $$symbolConvert(value){return $$rawSymbol($$toPrimitiveHint(value,'string'));}
function $$symbolForConvert(value){return $$rawSymbolFor($$toPrimitiveHint(value,'string'));}

function $$stringBuiltin(...args){if(args.length===0)return '';const value=args[0];if(typeof value==='symbol')return $$rawString(value);return $$stringConvert(value);}

function $$keyedGet(target,key,receiver){return $$getProperty(target,$$toPropertyKey(key),receiver);}
function $$inOperator(key,target){return Reflect.has(target,key);}
function $$stringConcat(value,...args){let out=$$rawString(value);for(let i=0;i<args.length;i++)out+=$$stringConvert(args[i]);return out;}
function $$numberBuiltin(...args){return args.length===0?0:$$rawNumber($$toPrimitiveHint(args[0],'number'));}
function $$constructDate(...args){if(args.length===1&&$$isObject(args[0])&&!$$isDate(args[0]))args[0]=$$toPrimitiveHint(args[0],'default');return $$rawDateConstruct(args);}

function $$stringIterator(value){$$requireCoercible(value);return $$builtinStringIterator($$stringConvert(value));}

// Class definition evaluates heritage.prototype before computed method keys.
function $$createClass(ctor,parent,derived,name){let proto;if(derived){if(parent!==null&&!$$isConstructor(parent))throw new TypeError('Class extends value is not a constructor or null');proto=parent===null?null:parent.prototype;if(proto!==null&&!$$isObject(proto))throw new TypeError('Class extends value has invalid prototype');}return $$classSetup(ctor,parent,derived,name,proto);}
function $$classKey(key){return $$toPropertyKey(key);}
function $$superSet(base,key,value,receiver){if(!$$reflectSet(base,key,value,receiver))throw new TypeError('Cannot assign super property');return value;}
function $$constructUsingTarget(target,values,newTarget){let proto;if(!$$isDerived(target))proto=newTarget.prototype;return $$constructReady(target,values,newTarget,proto);}

function $$dateSubclass(values){const proto=values.pop();let date;if(values.length===1)date=$$constructDate(values[0]);else date=$$rawDateConstruct(values);Object.setPrototypeOf(date,proto);return date;}

function $$boxedSubclass(kind,args,proto){let value;if(kind==='Number')value=args.length===0?0:$$numberBuiltin(args[0]);else if(kind==='String')value=args.length===0?'':$$stringConvert(args[0]);else value=args.length===0?false:Boolean(args[0]);return $$boxPrimitive(value,proto);}

function $$numberExponential(value,digits){return $$rawExponential(value,digits===undefined?undefined:Number(digits));}

// Async-from-sync adapts the iterator result's value, not an actual async
// iterator's value. All accessors and awaits remain ordinary durable frames.
function $$getAsyncIterator(value){
  $$requireCoercible(value);const method=value[Symbol.asyncIterator];
  if(method!==undefined&&method!==null)return $$getIteratorMethod(value,method);
  const rec=$$getIterator(value);
  const adapter={
    [Symbol.asyncIterator](){return this;},
    next(...args){try{if(this!==adapter)throw new TypeError('Invalid async iterator receiver');return $$asyncFromSyncResult($$invokeWithReceiver(rec.next,rec.iterator,args),rec,true);}catch(error){return Promise.reject(error);}},
    return(...args){try{if(this!==adapter)throw new TypeError('Invalid async iterator receiver');const method=rec.iterator.return;if(method===undefined||method===null)return Promise.resolve({value:args[0],done:true});return $$asyncFromSyncResult($$invokeWithReceiver(method,rec.iterator,args),rec,false);}catch(error){return Promise.reject(error);}},
    throw(...args){try{if(this!==adapter)throw new TypeError('Invalid async iterator receiver');const method=rec.iterator.throw;if(method===undefined||method===null){$$iteratorCloseNormal(rec);throw new TypeError('Iterator has no throw method');}return $$asyncFromSyncResult($$invokeWithReceiver(method,rec.iterator,args),rec,true);}catch(error){return Promise.reject(error);}}
  };return {iterator:adapter,next:adapter.next,done:false};
}
function $$asyncFromSyncResult(step,rec,closeOnRejection){
  return new Promise((resolve,reject)=>{
    if(!$$isObject(step))throw new TypeError('Iterator result must be an object');
    const done=!!step.done,normalized=$$awaitPromise(step.value);
    const promise=$$isPromise(normalized)?normalized:Promise.resolve(normalized);
    $$awaitSubscribe(promise,value=>resolve({value,done}),error=>{
      try{if(!done&&closeOnRejection)$$iteratorCloseThrow(rec,error);}catch(ignored){}
      reject(error);
    });
  });
}

// Await's PromiseResolve boundary runs accessors in normal serializable frames.
// The non-Promise thenable job has its own already-called flag so resolving,
// rejecting or throwing twice cannot change the original resolution.
function $$awaitPromise(value){
  if(!$$isObject(value))return value;
  if($$isPromise(value)&&value.constructor===Promise)return value;
  return new Promise((resolve,reject)=>$$resolveAwaitValue(value,resolve,reject));
}
function $$resolveAwaitValue(value,resolve,reject){
  if(!$$isObject(value)){resolve(value);return;}
  let then;
  try{then=value.then;}catch(error){reject(error);return;}
  if(typeof then!=='function'){resolve(value);return;}
  queueMicrotask(()=>{
    let called=false;
    const ok=x=>{if(called)return;called=true;$$resolveAwaitValue(x,resolve,reject);};
    const fail=x=>{if(called)return;called=true;reject(x);};
    Object.defineProperty(ok,'name',{value:''});
    Object.defineProperty(fail,'name',{value:''});
    try{$$invokeWithReceiver(then,value,[ok,fail]);}catch(error){fail(error);}
  });
}
async function $$asyncGeneratorAwaitReturn(value){return await value;}

// Locale input and option getters execute in ordinary, serializable VM frames.
function $$localeChoice(locales){if(locales===undefined)return 'en-US';if(typeof locales==='string')return locales;if(Array.isArray(locales)){for(const locale of locales)return $$stringConvert(locale);return 'en-US';}throw new TypeError('locales must be a string or an array');}
function $$localeOptions(options,allowed){if(options===undefined)return {};if(options===null)throw new TypeError('locale options must be an object');const obj=Object(options),keys=Object.keys(obj),out={};for(const key of keys){if(!allowed.includes(key))throw new RangeError('unsupported locale option: '+key);}for(const key of allowed){const value=obj[key];if(value!==undefined)out[key]=key==='numeric'||key==='hour12'?Boolean(value):$$stringConvert(value);}return out;}
function $$localeCompare(value,other,locales,options){$$requireCoercible(value);const a=$$stringConvert(value),b=$$stringConvert(other);return $$rawLocaleCompare(a,b,$$localeChoice(locales),$$localeOptions(options,['numeric','sensitivity','caseFirst']));}
function $$numberLocale(value,locales,options){return $$rawNumberLocale(value,$$localeChoice(locales),$$localeOptions(options,[]));}
function $$dateLocale(value,method,locales,options){return $$rawDateLocale(value,method,$$localeChoice(locales),$$localeOptions(options,['timeZone','hour12','year','month','day','hour','minute','second']));}

function $$encodeURI(value){return $$rawEncodeURI($$toPrimitiveHint(value,'string'));}
function $$decodeURI(value){return $$rawDecodeURI($$toPrimitiveHint(value,'string'));}
function $$encodeURIComponent(value){return $$rawEncodeURIComponent($$toPrimitiveHint(value,'string'));}
function $$decodeURIComponent(value){return $$rawDecodeURIComponent($$toPrimitiveHint(value,'string'));}
function $$stringRaw(template,...substitutions){$$requireCoercible(template);const raw=Object(template).raw;$$requireCoercible(raw);const obj=Object(raw);let n=Number(obj.length);if(Number.isNaN(n)||n<0)n=0;else n=Math.floor(n);if(n>1000000)throw new RangeError('String.raw segments exceed runtime limit');let out='';for(let i=0;i<n;i++){out=$$textLimit(out+$$stringConvert(obj[i]));if(i+1<n&&i<substitutions.length)out=$$textLimit(out+$$stringConvert(substitutions[i]));}return out;}
