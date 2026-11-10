// Run the independently executing Node engine against the same expectation
// fixtures extracted from tests_stage8.rs. Host/snapshot, budget errors, and
// deliberately restricted locale tests are excluded, not counted as passes.
const fs=require('node:fs'),vm=require('node:vm'),path=require('node:path');
(async()=>{
  const source=fs.readFileSync(path.join(__dirname,'../../src/agent_program/tests_stage8.rs'),'utf8');
  const pattern=/js_test!\(\s*(\w+)\s*,\s*(r#".*?"#|"(?:\\.|[^"\\])*")\s*,\s*json!\((.*?)\)\s*\);/gs;
  const excluded=new Set(['vm_stage8_regex_backtrack_error','vm_stage8_regex_backtrack_consumers','vm_stage8_locale_errors']);
  const cases=process.argv[2]?JSON.parse(fs.readFileSync(process.argv[2],'utf8')):
    [...source.matchAll(pattern)].filter(m=>!excluded.has(m[1])).map(m=>({name:m[1],source:m[2].startsWith('r#')?m[2].slice(3,-2):JSON.parse(m[2]),expected:JSON.parse(m[3])}));
  const results=[];
  for(const test of cases){
    let actual,error;
    try{actual=JSON.parse(JSON.stringify(await vm.runInNewContext(test.source+'\nmain();',{}, {timeout:1500})));}
    catch(e){error=String(e);}
    const pass=!error&&JSON.stringify(actual)===JSON.stringify(test.expected);
    results.push({name:test.name,pass,actual,expected:test.expected,error});
  }
  console.log(JSON.stringify({node:process.version,passed:results.filter(r=>r.pass).length,failed:results.filter(r=>!r.pass).length,results},null,2));
  if(results.some(r=>!r.pass))process.exitCode=1;
})();
