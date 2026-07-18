pub(super) const CANDIDATE: &str = r#"
const fs=require('fs'),path=require('path'),cp=require('child_process');
const payload=JSON.parse(process.env.A3S_STEP_INPUT_JSON||'{}');
const root='/tmp/a3s-bench-'+process.pid;
const candidate=path.join(root,'candidate'),workspace=path.join(root,'workspace');
const MAX_RESULT_BYTES=65536;
function safe(base,rel){
  if(typeof rel!=='string'||!rel||path.isAbsolute(rel)||rel.split('/').includes('..')) throw new Error('unsafe path: '+rel);
  const out=path.resolve(base,rel),prefix=path.resolve(base)+path.sep;
  if(!out.startsWith(prefix)) throw new Error('path escapes root: '+rel);
  return out;
}
function writeTree(base,tree){
  fs.mkdirSync(base,{recursive:true});
  for(const file of tree.files||[]){
    const target=safe(base,file.path);fs.mkdirSync(path.dirname(target),{recursive:true});
    fs.writeFileSync(target,Buffer.from(file.data,'base64'));
    fs.chmodSync(target,file.executable?0o700:0o600);
  }
}
function readTree(base){
  const files=[];
  function walk(dir){
    for(const entry of fs.readdirSync(dir,{withFileTypes:true})){
      const full=path.join(dir,entry.name),rel=path.relative(base,full).split(path.sep).join('/');
      if(entry.isDirectory()) walk(full);
      else if(entry.isFile()) files.push({path:rel,data:fs.readFileSync(full).toString('base64'),executable:(fs.statSync(full).mode&0o111)!==0});
      else throw new Error('workspace contains unsupported file: '+rel);
    }
  }
  walk(base);files.sort((a,b)=>a.path.localeCompare(b.path));return {files};
}
writeTree(candidate,payload.candidate);writeTree(workspace,payload.workspace);
const entry=safe(candidate,payload.entrypoint);
const run=cp.spawnSync('/bin/sh',[entry,workspace],{stdio:'inherit',timeout:payload.timeoutMs});
if(run.error) throw run.error;
if(run.status!==0) process.exit(run.status===null?1:run.status);
const resultJson=JSON.stringify(readTree(workspace));
if(Buffer.byteLength(resultJson,'utf8')>MAX_RESULT_BYTES) throw new Error('workspace result exceeds 64 KiB');
const result=Buffer.from(resultJson).toString('base64');
console.log('A3S_BENCH_RESULT_V1:'+result);
"#;

pub(super) const JUDGE: &str = r#"
import base64, importlib.util, json, os
from pathlib import Path

payload = json.loads(os.environ.get("A3S_STEP_INPUT_JSON", "{}"))
root = Path("/tmp") / ("a3s-bench-judge-" + str(os.getpid()))
MAX_RESULT_BYTES = 65536

def safe(base, relative):
    if not isinstance(relative, str) or not relative or relative.startswith("/") or ".." in relative.split("/"):
        raise ValueError("unsafe path: " + str(relative))
    target = (base / relative).resolve()
    if base.resolve() not in target.parents:
        raise ValueError("path escapes root: " + relative)
    return target

def write_tree(base, tree):
    base.mkdir(parents=True, exist_ok=True)
    for item in tree.get("files", []):
        target = safe(base, item["path"])
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(base64.b64decode(item["data"]))
        target.chmod(0o700 if item.get("executable") else 0o600)

judge_root, submission_root, hidden_root = root / "judge", root / "submission", root / "hidden"
write_tree(judge_root, payload["judge"])
write_tree(submission_root, payload["submission"])
write_tree(hidden_root, payload["hidden"])
entry_file = safe(judge_root, payload["entrypointFile"])
spec = importlib.util.spec_from_file_location("judge", entry_file)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
result = getattr(module, payload["entrypointFunction"])({
    "submission_root": str(submission_root),
    "hidden_bundle_root": str(hidden_root),
})
result_json = json.dumps(result, separators=(",", ":")).encode()
if len(result_json) > MAX_RESULT_BYTES:
    raise ValueError("Judge result exceeds 64 KiB")
encoded = base64.b64encode(result_json).decode()
print("A3S_BENCH_RESULT_V1:" + encoded)
"#;
