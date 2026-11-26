//! Cross-platform read-only host environment probes.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

pub const TOOLS: &[&str] = &["sys_disk_space", "sys_which", "sys_listening_ports", "sys_processes"];
pub fn is_tool(name: &str) -> bool { TOOLS.contains(&name) }
pub fn execute(name: &str, args: &Value) -> Result<Value> {
    match name { "sys_disk_space" => disk_space(args), "sys_which" => which(args), "sys_listening_ports" => listening_ports(args), "sys_processes" => processes(args), _ => anyhow::bail!("unknown sys probe: {name}") }
}

fn disk_space(args: &Value) -> Result<Value> {
    let path = Path::new(args["path"].as_str().or(args["workspace_root"].as_str()).unwrap_or(".")).canonicalize()?;
    #[cfg(unix)]
    let (total, available, free) = {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let raw=CString::new(path.as_os_str().as_bytes()).context("path contains NUL")?;
        let mut v:libc::statvfs=unsafe{std::mem::zeroed()};
        ensure!(unsafe{libc::statvfs(raw.as_ptr(),&mut v)}==0,"statvfs failed: {}",std::io::Error::last_os_error());
        let unit=if v.f_frsize==0{v.f_bsize as u64}else{v.f_frsize as u64};
        (v.f_blocks as u64*unit,v.f_bavail as u64*unit,v.f_bfree as u64*unit)
    };
    #[cfg(windows)]
    let (total, available, free) = {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide:Vec<u16>=path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let(mut a,mut t,mut f)=(0u64,0u64,0u64);
        ensure!(unsafe{GetDiskFreeSpaceExW(wide.as_ptr(),&mut a,&mut t,&mut f)}!=0,"GetDiskFreeSpaceExW failed: {}",std::io::Error::last_os_error()); (t,a,f)
    };
    #[cfg(not(any(unix,windows)))]
    let (total,available,free):(u64,u64,u64)=anyhow::bail!("disk query unsupported");
    let used=if total==0{0.0}else{(total.saturating_sub(free) as f64/total as f64*100.0).clamp(0.0,100.0)};
    let display_path = crate::tools::file_edit::display_path(&path);
    Ok(json!({"path":display_path,"total_bytes":total,"available_bytes":available,"free_bytes":free,"available_gb":available as f64/1_000_000_000.0,"used_percent":used}))
}
fn which(args:&Value)->Result<Value>{
    let names=args["names"].as_array().context("names must be an array")?;ensure!(names.len()<=32,"sys.which accepts at most 32 names");
    let dirs=std::env::var_os("PATH").map(|v|std::env::split_paths(&v).collect::<Vec<_>>()).unwrap_or_default();
    #[cfg(windows)] let exts:Vec<String>=std::env::var("PATHEXT").unwrap_or_else(|_|".COM;.EXE;.BAT;.CMD".into()).split(';').map(str::to_ascii_lowercase).collect();
    #[cfg(not(windows))] let exts:Vec<String>=vec![String::new()];
    let mut found=BTreeMap::new();
    for item in names { let n=item.as_str().context("command name must be a string")?;ensure!(!n.is_empty()&&!n.contains('/')&&!n.contains('\\')&&!n.contains('\0'),"sys.which expects bare names");
        let candidates=if Path::new(n).extension().is_some(){vec![n.to_owned()]}else{exts.iter().map(|e|if e.is_empty(){n.to_owned()}else{format!("{n}{e}")}).collect()};
        let path=dirs.iter().flat_map(|d|candidates.iter().map(move|c|d.join(c))).find(|p|p.is_file()&&executable(p)).and_then(|p|p.canonicalize().ok()).map(|p|crate::tools::file_edit::display_path(&p));found.insert(n.to_owned(),path);
    } Ok(json!({"found":found}))
}
#[cfg(unix)] fn executable(p:&Path)->bool{use std::os::unix::fs::PermissionsExt;std::fs::metadata(p).is_ok_and(|m|m.permissions().mode()&0o111!=0)}
#[cfg(windows)] fn executable(_p:&Path)->bool{true}

#[cfg(target_os="linux")]
fn listening_ports(args:&Value)->Result<Value>{
    let wanted=args["port"].as_u64().map(|v|v as u16);ensure!(args["port"].is_null()||args["port"].as_u64().is_some_and(|v|v<=65535),"port must be 0..65535");let local=args["loopback_only"]==true;let mut out=Vec::new();
    for(file,proto)in[("/proc/net/tcp","tcp"),("/proc/net/tcp6","tcp6")]{let text=match std::fs::read_to_string(file){Ok(v)=>v,Err(e)if e.kind()==std::io::ErrorKind::NotFound=>continue,Err(e)=>return Err(e.into())};for line in text.lines().skip(1){let c:Vec<_>=line.split_whitespace().collect();if c.get(3)!=Some(&"0A"){continue;}let Some((addr,hex))=c.get(1).and_then(|v|v.rsplit_once(':'))else{continue};let Ok(port)=u16::from_str_radix(hex,16)else{continue};if wanted.is_some_and(|v|v!=port){continue;}let ip=if proto=="tcp"{u32::from_str_radix(addr,16).ok().map(|v|std::net::Ipv4Addr::from(v.to_ne_bytes()).to_string())}else{decode_v6(addr)};let Some(ip)=ip else{continue};if local&&!ip.parse::<std::net::IpAddr>().is_ok_and(|v|v.is_loopback()){continue;}out.push(json!({"protocol":proto,"address":ip,"port":port}));}}
    out.sort_by_key(|v|(v["port"].as_u64(),v["protocol"].as_str().map(str::to_owned),v["address"].as_str().map(str::to_owned)));Ok(json!({"ports":out,"supported":true}))
}
#[cfg(target_os="linux")] fn decode_v6(raw:&str)->Option<String>{if raw.len()!=32{return None;}let mut b=[0u8;16];for(i,w)in raw.as_bytes().chunks_exact(8).enumerate(){b[i*4..i*4+4].copy_from_slice(&u32::from_str_radix(std::str::from_utf8(w).ok()?,16).ok()?.to_ne_bytes());}Some(std::net::Ipv6Addr::from(b).to_string())}
#[cfg(windows)]
fn listening_ports(args:&Value)->Result<Value>{
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetExtendedTcpTable,MIB_TCPROW_OWNER_PID,MIB_TCP6ROW_OWNER_PID,TCP_TABLE_OWNER_PID_LISTENER};use windows_sys::Win32::Networking::WinSock::{AF_INET,AF_INET6};
    let wanted=args["port"].as_u64().map(|v|v as u16);ensure!(args["port"].is_null()||args["port"].as_u64().is_some_and(|v|v<=65535),"port must be 0..65535");let local=args["loopback_only"]==true;let mut out=Vec::new();
    unsafe fn get(f:u32)->Result<Vec<u8>>{let mut n=0u32;let _=unsafe{GetExtendedTcpTable(std::ptr::null_mut(),&mut n,0,f,TCP_TABLE_OWNER_PID_LISTENER,0)};let mut b=vec![0u8;n as usize];let r=unsafe{GetExtendedTcpTable(b.as_mut_ptr().cast(),&mut n,0,f,TCP_TABLE_OWNER_PID_LISTENER,0)};ensure!(r==0,"GetExtendedTcpTable failed: {r}");Ok(b)}
    for(f,proto)in[(AF_INET as u32,"tcp"),(AF_INET6 as u32,"tcp6")]{let b=unsafe{get(f)}?;if b.len()<4{continue;}let n=u32::from_ne_bytes(b[..4].try_into()?)as usize;if f==AF_INET as u32{let rows=unsafe{std::slice::from_raw_parts(b.as_ptr().add(4).cast::<MIB_TCPROW_OWNER_PID>(),n.min(8192))};for r in rows{let port=u16::from_be(r.dwLocalPort as u16);let ip=std::net::Ipv4Addr::from(u32::from_be(r.dwLocalAddr)).to_string();if wanted.is_some_and(|p|p!=port)||local&&!ip.parse::<std::net::IpAddr>().is_ok_and(|v|v.is_loopback()){continue;}out.push(json!({"protocol":proto,"address":ip,"port":port,"pid":r.dwOwningPid}));}}else{let rows=unsafe{std::slice::from_raw_parts(b.as_ptr().add(4).cast::<MIB_TCP6ROW_OWNER_PID>(),n.min(8192))};for r in rows{let port=u16::from_be(r.dwLocalPort as u16);let ip=std::net::Ipv6Addr::from(r.ucLocalAddr).to_string();if wanted.is_some_and(|p|p!=port)||local&&!ip.parse::<std::net::IpAddr>().is_ok_and(|v|v.is_loopback()){continue;}out.push(json!({"protocol":proto,"address":ip,"port":port,"pid":r.dwOwningPid}));}}}Ok(json!({"ports":out,"supported":true}))
}
#[cfg(target_os="macos")]
fn listening_ports(args:&Value)->Result<Value>{
    let wanted=args["port"].as_u64().map(|v|v as u16);ensure!(args["port"].is_null()||args["port"].as_u64().is_some_and(|v|v<=65535),"port must be 0..65535");let local=args["loopback_only"]==true;
    let output=std::process::Command::new("/usr/sbin/lsof").args(["-nP","-iTCP","-sTCP:LISTEN","-F","pcn"]).output();
    let Ok(output)=output else { return Ok(json!({"ports":[],"supported":false})); };
    if !output.status.success() && !output.stdout.is_empty() { anyhow::bail!("lsof failed"); }
    let text=String::from_utf8_lossy(&output.stdout);
    let mut out=Vec::new();
    let mut pid:Option<u32>=None;
    let mut proc_name:Option<String>=None;
    for line in text.lines() {
        if let Some(v)=line.strip_prefix('p') { pid=v.parse().ok(); proc_name=None; }
        else if let Some(v)=line.strip_prefix('c') { proc_name=Some(v.to_owned()); }
        else if let Some(v)=line.strip_prefix('n') {
            let Some((raw_addr,raw_port))=v.rsplit_once(':') else { continue; };
            let Ok(port)=raw_port.parse::<u16>() else { continue; };
            if wanted.is_some_and(|p|p!=port) { continue; }
            let ip=raw_addr.trim_start_matches('[').trim_end_matches(']');
            let proto=if raw_addr.starts_with('[') || ip.contains(':') { "tcp6" } else { "tcp" };
            let addr=if ip=="*" { if proto=="tcp6" { "::" } else { "0.0.0.0" } } else { ip };
            if local && !addr.parse::<std::net::IpAddr>().is_ok_and(|a|a.is_loopback()) { continue; }
            let mut row=json!({"protocol":proto,"address":addr,"port":port});
            if let Some(p)=pid { row["pid"]=json!(p); }
            if let Some(ref c)=proc_name { row["process"]=json!(c); }
            out.push(row);
        }
    }
    out.sort_by_key(|v|(v["port"].as_u64(),v["protocol"].as_str().map(str::to_owned),v["address"].as_str().map(str::to_owned)));
    Ok(json!({"ports":out,"supported":true}))
}
#[cfg(not(any(target_os="linux",target_os="macos",windows)))] fn listening_ports(_:&Value)->Result<Value>{Ok(json!({"ports":[],"supported":false}))}

fn process_filter(args:&Value)->Result<String>{
    let filter=args["filter"].as_str().unwrap_or("");
    ensure!(filter.chars().count()<=256,"process filter exceeds 256 characters");
    Ok(filter.to_lowercase())
}

#[cfg(target_os="macos")]
#[link(name="proc")]
unsafe extern "C" {
    fn proc_listpids(kind:u32,type_info:u32,buffer:*mut std::ffi::c_void,buffersize:i32)->i32;
    fn proc_name(pid:i32,buffer:*mut std::ffi::c_void,buffersize:u32)->i32;
}
#[cfg(target_os="macos")]
fn processes(args:&Value)->Result<Value>{
    let needed=unsafe{proc_listpids(0,0,std::ptr::null_mut(),0)}.max(0) as usize;
    let mut pids=vec![0i32;(needed/4+16).min(32768)];
    let read=unsafe{proc_listpids(0,0,pids.as_mut_ptr().cast(),(pids.len()*4) as i32)}.max(0) as usize;
    let system_truncated=needed/4>pids.len() || read/4>pids.len();
    pids.truncate((read/4).min(pids.len()));
    let limit=args["limit"].as_u64().unwrap_or(200).clamp(1,1000) as usize;
    let filter=process_filter(args)?;
    let mut out=Vec::new();
    for pid in pids.into_iter().filter(|pid|*pid>0) {
        let mut name=[0i8;256];
        if unsafe{proc_name(pid,name.as_mut_ptr().cast(),name.len() as u32)}<=0 { continue; }
        let end=name.iter().position(|v|*v==0).unwrap_or(name.len());
        let name=String::from_utf8_lossy(unsafe{std::slice::from_raw_parts(name.as_ptr().cast::<u8>(),end)}).into_owned();
        if name.to_lowercase().contains(&filter) { out.push(json!({"pid":pid as u32,"name":name})); }
    }
    out.sort_by_key(|v|v["pid"].as_u64());
    let truncated=system_truncated || out.len()>limit;
    out.truncate(limit);
    Ok(json!({"processes":out,"truncated":truncated,"supported":true}))
}

#[cfg(target_os="linux")]
fn processes(args:&Value)->Result<Value>{
    let limit=args["limit"].as_u64().unwrap_or(200).clamp(1,1000) as usize;
    let filter=process_filter(args)?;
    let mut out=Vec::new();
    for entry in std::fs::read_dir("/proc")?.flatten() {
        let Some(pid)=entry.file_name().to_str().and_then(|value|value.parse::<u32>().ok()) else { continue; };
        let name=std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default().trim().to_owned();
        if name.is_empty() || !name.to_lowercase().contains(&filter) { continue; }
        let stat=std::fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
        let tail=stat.rsplit_once(')').map(|(_,value)|value.trim()).unwrap_or("");
        let fields:Vec<_>=tail.split_whitespace().collect();
        out.push(json!({"pid":pid,"ppid":fields.get(1).and_then(|value|value.parse::<u32>().ok()),"name":name,"state":fields.first()}));
    }
    out.sort_by_key(|value|value["pid"].as_u64());
    let truncated=out.len()>limit;
    out.truncate(limit);
    Ok(json!({"processes":out,"truncated":truncated,"supported":true}))
}

#[cfg(windows)]
fn processes(args:&Value)->Result<Value>{
    use std::os::windows::io::{AsRawHandle,FromRawHandle};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    let limit=args["limit"].as_u64().unwrap_or(200).clamp(1,1000) as usize;
    let filter=process_filter(args)?;
    let raw=unsafe{CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS,0)};
    ensure!(raw!=INVALID_HANDLE_VALUE,"process snapshot failed");
    let handle=unsafe{std::os::windows::io::OwnedHandle::from_raw_handle(raw)};
    let mut entry:PROCESSENTRY32W=unsafe{std::mem::zeroed()};
    entry.dwSize=std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut out=Vec::new();
    let mut ok=unsafe{Process32FirstW(handle.as_raw_handle(),&mut entry)!=0};
    while ok {
        let end=entry.szExeFile.iter().position(|value|*value==0).unwrap_or(entry.szExeFile.len());
        let name=String::from_utf16_lossy(&entry.szExeFile[..end]);
        if name.to_lowercase().contains(&filter) { out.push(json!({"pid":entry.th32ProcessID,"ppid":entry.th32ParentProcessID,"name":name})); }
        ok=unsafe{Process32NextW(handle.as_raw_handle(),&mut entry)!=0};
    }
    let truncated=out.len()>limit;
    out.truncate(limit);
    Ok(json!({"processes":out,"truncated":truncated,"supported":true}))
}

#[cfg(not(any(target_os="linux",target_os="macos",windows)))]
fn processes(_:&Value)->Result<Value>{Ok(json!({"processes":[],"truncated":false,"supported":false}))}

pub fn definitions()->Vec<Value>{vec![
json!({"name":"sys_disk_space","description":"Get filesystem capacity using native OS APIs; replaces df -h.","inputSchema":{"type":"object","properties":{"path":{"type":"string","default":"."}}},"outputSchema":{"type":"object","required":["path","total_bytes","available_bytes","free_bytes","available_gb","used_percent"],"properties":{"path":{"type":"string"},"total_bytes":{"type":"integer"},"available_bytes":{"type":"integer"},"free_bytes":{"type":"integer"},"available_gb":{"type":"number"},"used_percent":{"type":"number"}}}}),
json!({"name":"sys_which","description":"Find up to 32 executable names on PATH without starting a subprocess; honors PATHEXT on Windows.","inputSchema":{"type":"object","required":["names"],"properties":{"names":{"type":"array","items":{"type":"string"},"maxItems":32}}},"outputSchema":{"type":"object","required":["found"],"properties":{"found":{"type":"object","additionalProperties":{"type":["string","null"]}}}}}),
json!({"name":"sys_listening_ports","description":"List TCP listeners from native OS tables. Unsupported platforms return supported:false.","inputSchema":{"type":"object","properties":{"port":{"type":"integer","minimum":0,"maximum":65535},"loopback_only":{"type":"boolean","default":false}}},"outputSchema":{"type":"object","required":["ports","supported"],"properties":{"ports":{"type":"array","items":{"type":"object","required":["protocol","address","port"],"properties":{"protocol":{"enum":["tcp","tcp6"]},"address":{"type":"string"},"port":{"type":"integer"},"pid":{"type":"integer"},"process":{"type":"string"}}}},"supported":{"type":"boolean"}}}}),
json!({"name":"sys_processes","description":"List bounded process ID, parent ID, short name and state only. Command lines and arguments are never read or returned.","inputSchema":{"type":"object","properties":{"filter":{"type":"string","maxLength":256},"limit":{"type":"integer","minimum":1,"maximum":1000,"default":200}}},"outputSchema":{"type":"object","required":["processes","truncated","supported"],"properties":{"processes":{"type":"array","items":{"type":"object","required":["pid","name"],"properties":{"pid":{"type":"integer"},"ppid":{"type":"integer"},"name":{"type":"string"},"state":{"type":"string"}}}},"truncated":{"type":"boolean"},"supported":{"type":"boolean"}}}})
]}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn native_disk_probe_reports_consistent_capacities() {
        let result=disk_space(&json!({"path":"."})).unwrap();
        assert!(result["total_bytes"].as_u64().unwrap()>0);
        assert!(result["available_bytes"].as_u64().unwrap()<=result["total_bytes"].as_u64().unwrap());
        assert!(result["used_percent"].as_f64().unwrap().is_finite());
    }

    #[test]
    fn which_resolves_path_and_reports_missing_as_null() {
        let result=which(&json!({"names":[if cfg!(windows){"git.exe"}else{"git"},"__definitely_missing_binary_xyz__"]})).unwrap();
        let found=&result["found"];
        assert!(found[if cfg!(windows){"git.exe"}else{"git"}].as_str().is_some());
        assert!(found["__definitely_missing_binary_xyz__"].is_null());
        assert!(which(&json!({"names":["../git"]})).is_err());
        assert!(which(&json!({"names":vec!["git";33]})).is_err());
    }

    #[test]
    #[cfg(any(target_os = "linux", windows))]
    fn listener_probe_finds_a_bound_loopback_port() {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();
        let port=listener.local_addr().unwrap().port();
        let result=listening_ports(&json!({"port":port,"loopback_only":true})).unwrap();
        assert_eq!(result["supported"],true);
        assert!(result["ports"].as_array().unwrap().iter().any(|entry|entry["port"]==port));
        drop(listener);
    }

#[cfg(not(any(target_os = "linux", windows)))]
    #[test]
    fn unsupported_listener_probe_degrades_without_error() {
        let result=listening_ports(&json!({})).unwrap();
        assert_eq!(result["supported"],false);
        assert_eq!(result["ports"].as_array().unwrap().len(),0);
    }

    #[test]
    fn process_probe_includes_test_process_without_command_line() {
        let pid=std::process::id();
        let result=processes(&json!({"filter":"","limit":1000})).unwrap();
        assert_eq!(result["supported"],true);
        let process=result["processes"].as_array().unwrap().iter().find(|entry|entry["pid"]==pid).expect("test process appears in native process table");
        assert!(process["name"].as_str().is_some_and(|name|!name.is_empty()));
        assert!(process.get("cmdline").is_none());
        assert!(process.get("args").is_none());
        assert!(processes(&json!({"filter":"x".repeat(257)})).is_err());
    }
}
