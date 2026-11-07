'use strict';
function main(){function C(n){this.n=n;}C.prototype.read=function(){return this.n;};const Bound=C.bind({n:99},7),a=new Bound(),b=Object.create(C.prototype);return [a.n,a.read(),a instanceof C,a instanceof Bound,b instanceof C,C.prototype.constructor===C,Object.hasOwn(C,'prototype'),Object.hasOwn(Bound,'prototype'),Object.hasOwn(()=>{},'prototype'),Object.keys(C),Object.keys(C.prototype)];}
console.log(JSON.stringify(main()));
