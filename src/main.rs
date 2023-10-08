use caps::errors::CapsError;
use caps::{has_cap, CapSet, Capability};
use nix::errno::Errno;
use nix::sched::setns;
use nix::{
    sched::CloneFlags,
    sys::wait::{waitpid, WaitStatus},
    unistd::{execv, fork, seteuid, ForkResult},
};
use rc_zip::prelude::ReadZip;
use std::ffi::{CString, NulError};
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::{
    collections::HashMap,
    env::{self, current_exe},
    fs::File,
    io::{self, BufRead},
    os::fd::AsRawFd,
    process,
};
use sysinfo::{PidExt, ProcessExt, System, SystemExt};


#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Zip(rc_zip::Error),
    Null(NulError),
    Caps(CapsError),
    Errno(&'static str, Errno),
}

// there exists a `derive_more` crate that can help remove
// this sort of boilerplate.

impl From<io::Error> for Error {
    fn from(inner: io::Error) -> Self {
        Self::Io(inner)
    }
}

impl From<rc_zip::Error> for Error {
    fn from(inner: rc_zip::Error) -> Self {
        Self::Zip(inner)
    }
}

impl From<NulError> for Error {
    fn from(inner: NulError) -> Self {
        Self::Null(inner)
    }
}

impl From<CapsError> for Error {
    fn from(inner: CapsError) -> Self {
        Self::Caps(inner)
    }
}

impl From<(&'static str, Errno)> for Error {
    fn from((message, errno): (&'static str, Errno)) -> Self {
        Self::Errno(message, errno)
    }
}

fn main() -> Result<(), Error> {
    // check out the `clap` crate and it's `derive` feature.
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
    check_perms()?;

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

            if let Err(e) = fork_scanner(**uid, pid.as_u32(), cmd) {
                println!("{:?}", e);
            }
        }
    }

    Ok(())
}

fn match_first(strings: &[String], search: &str) -> bool {
    if strings.is_empty() {
        return false;
    }

    strings[0].contains(search)
}

// Simple example that finds a classpath and looks for name and version info
fn scan(args: Vec<String>) -> Result<(), Error> {
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
                if Path::new(path).is_file() {
                    analyze_manifest(path, &mut map)?;
                }
            }
        }
        i += 1
    }

    for (key, value) in map {
        println!("FOUND: {key} - {value}")
    }

    Ok(())
}

fn analyze_manifest(path: &str, hits: &mut HashMap<String, String>) -> Result<(), rc_zip::Error> {
    let zipfile = File::open(path).unwrap();
    let arc = zipfile.read_zip()?;
    if let Some(entry) = arc.by_name("META-INF/MANIFEST.MF") {
        let mut map = HashMap::new();
        let lines = io::BufReader::new(entry.reader()).lines();
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
    } else {
        println!("no manifest found");
    }

    Ok(())
}

fn check_perms() -> Result<(), Error> {
    let suid = has_cap(None, CapSet::Effective, Capability::CAP_SETUID)?;
    let sys = has_cap(None, CapSet::Effective, Capability::CAP_SYS_ADMIN)
        .expect("Error reading CAP_SYS_ADMIN");

    if !suid {
        return Err(CapsError::from("you must have CAP_SETUID").into());
    }

    if !sys {
        return Err(CapsError::from("you must have CAP_SYS_ADMIN").into());
    }

    Ok(())
}

fn program_dir() -> Result<PathBuf, io::Error> {
    let mut path = current_exe()?;
    path.pop();
    Ok(path)
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

fn join_namespace(pid: u32) -> Result<(), Error> {
    let s = format!("/proc/{}/ns/mnt", pid);
    let f = File::open(&s).expect("could not open mount namespace");
    let fd: BorrowedFd<'_> = f.as_fd();

    println!("Setting ns: {s}");
    if let Err(e) = setns(fd, CloneFlags::CLONE_NEWNS) {
        Err(Error::from(("could not switch to mount ns", e)))
    } else {
        Ok(())
    }
}

fn switch_user(uid: u32) -> Result<(), Error> {
    println!("Becoming user: {uid}");
    if let Err(e) = seteuid(uid.into()) {
        Err(("could not switch to uid {uid}", e).into())
    } else {
        Ok(())
    }
}

fn open_directory() -> Result<File, io::Error> {
    println!("Setting up directory FD");

    File::open(program_dir()?)
}

fn self_exec(dir_fd: File, program: String, cmd: &[String]) -> Result<(), Error> {
    println!("Self executing...");

    if let Err(e) = nix::unistd::fchdir(dir_fd.as_raw_fd()) {
        println!("Error: could not change to orphaned program dir: {e}");
        process::exit(1)
    }

    let path = CString::new(format!("./{}", program))?;

    let mut cvec = Vec::with_capacity(2 + cmd.len());
    cvec.push(path.clone());
    cvec.push(CString::new("scan")?);

    for s in cmd {
        cvec.push(CString::new(s.as_str())?);
    }

    if let Err(e) = execv(&path, cvec.as_slice()) {
        println!("Error: could not self-exec {:?}: {e}", path);
        process::exit(1);
    }

    Ok(())
}

fn fork_scanner(uid: u32, pid: u32, cmd: &[String]) -> Result<(), Error> {
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
            let dir_fd = open_directory()?;

            // Swap the child processes mount namespace with the container
            join_namespace(pid)?;

            // Become the same user that is running the process
            switch_user(uid)?;

            // Rexecute ourself, so that scanning operations can be safely performed:
            // unprivileged and using threads if necessary.
            //
            // This could also be replaced with another non-Rust program, provided it
            // is staticly linked.
            self_exec(dir_fd, program, cmd)?;
        }
        Err(e) => println!("Error: Fork failed: {e}"),
    }

    Ok(())
}
