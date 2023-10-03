use caps::{has_cap, CapSet, Capability};
use nix::sched::setns;
use nix::{
    sched::CloneFlags,
    sys::wait::{waitpid, WaitStatus},
    unistd::{execv, fork, seteuid, ForkResult},
};
use std::ffi::CString;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;
use std::{
    collections::HashMap,
    env::{self, current_exe},
    fs::File,
    io::{self, BufRead},
    os::fd::AsRawFd,
    process,
};
use sysinfo::{PidExt, ProcessExt, System, SystemExt};
use zip::{self, ZipArchive};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() > 1 && args[1] == "scan" {
        println!("Scanning...");

        // All code within scan is safe to do anything it wants and is unprivileged
        return scan(args);
    }

    // IMPORTANT: All call paths beyond this point needs to follow strict rules:
    // 1. Extreme care for every operation should be followed since this is privileged code,
    //    effectively running as root
    // 2. Since this process will fork, no threads can be created for the *entire life*
    //    of the process
    // 3. Thirdparty code should be avoided if at all possible, since using it makes
    //    achieving 1 and 2 very difficult. In cases where necessary, all code should be
    //    audited

    // Verify this code is running with root level perms
    check_perms();

    // For caution (see rules 2,3) this should be replaced with manual proc scanning
    let s = System::new_all();
    let processes = s.processes();

    for (pid, process) in processes {
        let cmd = process.cmd();
        if match_first(cmd, "java") {
            let uid: &sysinfo::Uid = process.user_id().unwrap();
            println!(
                "found java! [{}] uid [{}] cmd: {}",
                pid,
                **uid,
                cmd.join(" ")
            );
            fork_scanner(**uid, pid.as_u32(), cmd)
        }
    }
}

fn match_first(strings: &[String], search: &str) -> bool {
    if strings.is_empty() {
        return false;
    }

    strings[0].contains(search)
}

// Simple example that finds a classpath and looks for name and version info
fn scan(args: Vec<String>) {
    let mut i = 0;
    let mut map = HashMap::new();
    loop {
        if i + 1 >= args.len() {
            break;
        }

        let arg = args.get(i).unwrap();

        if arg == "-classpath" || arg == "--class-path" {
            i += 1;
            let paths = args.get(i).unwrap().split(':');
            for path in paths {
                let p = Path::new(path);
                if p.is_file() {
                    analyze_manifest(path, &mut map);
                }
            }
        }
        i += 1
    }

    for (key, value) in map {
        println!("FOUND: {key} - {value}")
    }
}

fn analyze_manifest(path: &str, hits: &mut HashMap<String, String>) {
    let zipfile = std::fs::File::open(path).unwrap();
    let mut arc = ZipArchive::new(zipfile).expect("ahh zips");
    let file = match arc.by_name("META-INF/MANIFEST.MF") {
        Ok(file) => file,
        Err(..) => {
            println!("no manifest found");
            return;
        }
    };

    let mut map = HashMap::new();
    let lines = io::BufReader::new(file).lines();
    for line in lines.flatten() {
        let c: Vec<&str> = line.split(':').collect();

        if c.len() > 1 {
            let key = c.first().unwrap().trim();
            let value = c.get(1).unwrap().trim();
            map.insert(key.to_string(), value.to_string());
        }
    }

    if let Some(title) = map.get("Implementation-Title").or(map.get("Bundle-Name")) {
        let version = match map.get("Implementation-Version") {
            Some(v) => v,
            None => "no version",
        };

        hits.insert(title.to_string(), version.to_string());
    }
}

fn check_perms() {
    let suid =
        has_cap(None, CapSet::Effective, Capability::CAP_SETUID).expect("Error reading CAP_SETUID");
    let sys = has_cap(None, CapSet::Effective, Capability::CAP_SYS_ADMIN)
        .expect("Error reading CAP_SYS_ADMIN");

    if !suid {
        println!("Error: you must have CAP_SETUID! (are you root?)");
        process::exit(1)
    }

    if !sys {
        println!("Error: you must have CAP_SYS_ADMIN! (are you root?)");
        process::exit(1)
    }
}

fn program_dir() -> std::path::PathBuf {
    let mut path = current_exe().expect("could not determine program dir");
    path.pop();
    path
}

fn program_name() -> String {
    let path = current_exe().expect("could not determine program dir");
    return path
        .file_name()
        .unwrap()
        .to_os_string()
        .into_string()
        .unwrap();
}

fn join_namespace(pid: u32) {
    let s = format!("/proc/{}/ns/mnt", pid);
    let f = File::open(&s).expect("could not open mount namespace");
    let fd: BorrowedFd<'_> = f.as_fd();

    println!("Setting ns: {s}");
    if let Err(e) = setns(fd, CloneFlags::CLONE_NEWNS) {
        println!("error: could not switch to mount ns: {e}");
        process::exit(1)
    }
}

fn switch_user(uid: u32) {
    println!("Becoming user: {uid}");
    if let Err(e) = seteuid(uid.into()) {
        println!("error: could not switch to uid {uid}: {e}");
        process::exit(1)
    }
}

fn open_directory() -> File {
    println!("Setting up directory FD");

    match File::open(program_dir()) {
        Err(e) => {
            println!("Error: could not open program dir: {e}");
            process::exit(1);
        }
        Ok(f) => f,
    }
}

fn self_exec(dir_fd: File, program: String, cmd: &[String]) {
    println!("Self executing...");

    if let Err(e) = nix::unistd::fchdir(dir_fd.as_raw_fd()) {
        println!("Error: could not change to orphaned program dir: {e}");
        process::exit(1)
    }

    let path = CString::new(format!("./{}", program)).unwrap();

    let mut cvec = Vec::with_capacity(2 + cmd.len());
    cvec.push(path.clone());
    cvec.push(CString::new("scan").unwrap());

    for s in cmd {
        cvec.push(CString::new(s.as_str()).unwrap());
    }

    if let Err(e) = execv(&path, cvec.as_slice()) {
        println!("Error: could not self-exec {}: {e}", path.to_str().unwrap());
        process::exit(1);
    }
}

fn fork_scanner(uid: u32, pid: u32, cmd: &[String]) {
    match unsafe { fork() } {
        Ok(ForkResult::Parent { child, .. }) => {
            println!(
                "Continuing execution in parent process, new child has pid: {}",
                child
            );
            match waitpid(child, None) {
                Err(e) => println!("Error: problem waiting for child: {e}"),
                Ok(w) => match w {
                    WaitStatus::Exited(_, code) if code == 0 => {}
                    WaitStatus::Exited(_, code) if code != 0 => {
                        println!("Error: problem with child: returned {code}")
                    }
                    _ => println!("Error: problem with child: {:?}", w),
                },
            }
        }
        Ok(ForkResult::Child) => {
            // Requires proc so must be called before joining the namsepace
            let program = program_name();

            // Hold open the program directoy so that it can be utilized as an orphan outside
            // the container FS
            let dir_fd = open_directory();

            // Swap the child processes mount namespace with the container
            join_namespace(pid);

            // Become the same user that is running the process
            switch_user(uid);

            // Rexecute ourself, so that scanning operations can be safely performed:
            // unprivileged and using threads if necessary.
            //
            // This could also be replaced with another non-Rust program, provided it
            // is staticly linked.
            self_exec(dir_fd, program, cmd);
        }
        Err(e) => println!("Error: Fork failed: {e}"),
    }
}
