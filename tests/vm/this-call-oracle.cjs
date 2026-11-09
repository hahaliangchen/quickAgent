'use strict';
function main(){function sum(a,b,c){return [this.n,a,b,c];}const first={n:7},other={n:8};const bound=sum.bind(first,1),twice=bound.bind(other,2);return [sum.call(first,1,2,3),sum.apply(first,{0:1,1:2,2:3,length:3}),sum.apply(first,null),twice(3),twice.call(other,4),bound!==sum,bound instanceof Function,typeof bound,JSON.stringify({fn:bound})];}
console.log(JSON.stringify(main()));
