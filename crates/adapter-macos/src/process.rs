//! Process inspection and termination on macOS.

use std::path::PathBuf;

use runtime_adapter::generic::GenericProcessProvider;
use runtime_adapter::process::{ProcessIdentity, ProcessInfo, ProcessProvider, TerminationMode};
use runtime_types::{Result, RuntimeError, StopSignal};

#[derive(Debug, Default)]
pub struct MacProcessProvider {
    generic: GenericProcessProvider,
}

impl MacProcessProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Exact start time from `proc_pidinfo(PROC_PIDTBSDINFO)`, in milliseconds.
    ///
    /// Falls back to the `sysinfo` value when the call fails, which happens for
    /// processes owned by another user.
    fn precise_start_time_ms(pid: u32) -> Option<i64> {
        // SAFETY: `proc_pidinfo` writes at most `size_of::<proc_bsdinfo>()`
        // bytes into `info`, and we pass exactly that size. A short or failed
        // read is reported through the return value, which we check.
        unsafe {
            let mut info: libc::proc_bsdinfo = std::mem::zeroed();
            let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
            let written = libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            );
            if written != size {
                return None;
            }
            Some((info.pbi_start_tvsec as i64) * 1000 + (info.pbi_start_tvusec as i64) / 1000)
        }
    }

    /// Working directory from `proc_pidinfo(PROC_PIDVNODEPATHINFO)`.
    ///
    /// This is the pid -> cwd -> project link the whole product rests on, and
    /// it has to be native: `sysinfo` returns `None` for cwd on macOS, so
    /// without this call no listening port could ever be traced to a project.
    ///
    /// Returns `None` for processes owned by another user, which the kernel
    /// refuses to describe without root.
    fn cwd(pid: u32) -> Option<PathBuf> {
        // SAFETY: `proc_pidinfo` writes at most `size_of::<proc_vnodepathinfo>()`
        // bytes into `info`, which is exactly the size we declare. Anything
        // short of a full write is reported by the return value.
        unsafe {
            let mut info: libc::proc_vnodepathinfo = std::mem::zeroed();
            let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
            let written = libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            );
            if written != size {
                return None;
            }
            // `vip_path` is a fixed MAXPATHLEN buffer stored as chunks of 32
            // chars; flattening and stopping at the NUL yields the real path.
            let bytes: Vec<u8> = info
                .pvi_cdir
                .vip_path
                .iter()
                .flatten()
                .take_while(|byte| **byte != 0)
                .map(|byte| *byte as u8)
                .collect();
            if bytes.is_empty() {
                return None;
            }
            Some(PathBuf::from(String::from_utf8_lossy(&bytes).to_string()))
        }
    }

    /// Full argv from `sysctl(KERN_PROCARGS2)`.
    ///
    /// `sysinfo` reports an empty command line on macOS, which would leave the
    /// GUI unable to say *what* an unregistered process on a port actually is.
    ///
    /// The call needs an `ARGMAX`-sized buffer (a megabyte), so it is made only
    /// for single-process lookups, never while walking the whole process table.
    fn command_line(pid: u32) -> Option<Vec<String>> {
        // SAFETY: both sysctl calls pass a mib of the length they declare and a
        // buffer of the size they declare; sizes are updated in place by the
        // kernel and every read below is bounds-checked against the result.
        unsafe {
            let mut argmax: libc::c_int = 0;
            let mut size = std::mem::size_of::<libc::c_int>();
            let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
            if libc::sysctl(
                mib.as_mut_ptr(),
                2,
                &mut argmax as *mut _ as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            ) != 0
                || argmax <= 0
            {
                return None;
            }

            let mut buffer = vec![0u8; argmax as usize];
            let mut size = buffer.len();
            let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
            if libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buffer.as_mut_ptr() as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            ) != 0
            {
                // EPERM for another user's process, EINVAL once it has exited.
                return None;
            }
            buffer.truncate(size);
            Some(parse_procargs(&buffer))
        }
    }

    /// The environment of a running process.
    ///
    /// One `sysctl` with an `ARGMAX` buffer, as for argv, so it is made only
    /// for single-process lookups.
    fn environment(pid: u32) -> Option<Vec<(String, String)>> {
        // SAFETY: as in `command_line` — mib lengths match, sizes are updated
        // in place by the kernel, and reads are bounds-checked.
        unsafe {
            let mut argmax: libc::c_int = 0;
            let mut size = std::mem::size_of::<libc::c_int>();
            let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
            if libc::sysctl(
                mib.as_mut_ptr(),
                2,
                &mut argmax as *mut _ as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            ) != 0
                || argmax <= 0
            {
                return None;
            }

            let mut buffer = vec![0u8; argmax as usize];
            let mut size = buffer.len();
            let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
            if libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buffer.as_mut_ptr() as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            ) != 0
            {
                return None;
            }
            buffer.truncate(size);
            Some(parse_procenv(&buffer))
        }
    }

    fn refine(mut info: ProcessInfo) -> ProcessInfo {
        if let Some(start_time_ms) = Self::precise_start_time_ms(info.pid) {
            info.start_time_ms = start_time_ms;
        }
        if info.cwd.is_none() {
            info.cwd = Self::cwd(info.pid);
        }
        info
    }

    /// The process group id, or `None` if the process is gone.
    fn process_group(pid: u32) -> Option<i32> {
        // SAFETY: `getpgid` only reads kernel state for the given pid.
        let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
        if pgid < 0 {
            None
        } else {
            Some(pgid)
        }
    }

    fn signal_group(pgid: i32, signal: libc::c_int) -> std::io::Result<()> {
        // SAFETY: a plain `killpg` on a group id we just read back.
        let result = unsafe { libc::killpg(pgid, signal) };
        if result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

impl ProcessProvider for MacProcessProvider {
    fn list_processes(&self) -> Result<Vec<ProcessInfo>> {
        Ok(self
            .generic
            .list_processes()?
            .into_iter()
            .map(Self::refine)
            .collect())
    }

    fn environment(&self, pid: u32, keys: &[&str]) -> Result<Option<Vec<(String, String)>>> {
        let Some(all) = Self::environment(pid) else {
            return Ok(None);
        };
        Ok(Some(
            all.into_iter()
                .filter(|(key, _)| keys.contains(&key.as_str()))
                .collect(),
        ))
    }

    fn process_info(&self, pid: u32) -> Result<Option<ProcessInfo>> {
        Ok(self.generic.process_info(pid)?.map(|info| {
            let mut info = Self::refine(info);
            if info.command_line.is_empty() {
                if let Some(argv) = Self::command_line(info.pid) {
                    info.command_line = argv;
                }
            }
            info
        }))
    }

    fn terminate_tree(&self, identity: &ProcessIdentity, mode: TerminationMode) -> Result<bool> {
        // Re-verify identity immediately before signalling: between the caller
        // reading state and this call, the pid could have been recycled.
        let Some(current) = self.process_info(identity.pid)? else {
            return Ok(false);
        };
        if !current.identity().matches(identity) {
            return Ok(false);
        }

        let signal = match mode {
            TerminationMode::Graceful(StopSignal::Term) => libc::SIGTERM,
            TerminationMode::Graceful(StopSignal::Int) => libc::SIGINT,
            TerminationMode::Graceful(StopSignal::Quit) => libc::SIGQUIT,
            TerminationMode::Graceful(StopSignal::Hup) => libc::SIGHUP,
            TerminationMode::Forceful => libc::SIGKILL,
        };

        // Services are spawned into their own process group, so signalling the
        // group reaches every descendant in one call and cannot race a fork.
        match Self::process_group(identity.pid) {
            Some(pgid) if pgid as u32 == identity.pid => {
                Self::signal_group(pgid, signal).map_err(|err| {
                    RuntimeError::io(format!("killpg({pgid}) failed: {err}"))
                })?;
                Ok(true)
            }
            // Adopted processes we did not spawn are not group leaders; walk
            // the tree instead of signalling a group we do not own.
            _ => self.generic.terminate_tree(identity, mode),
        }
    }
}

/// Decode a `KERN_PROCARGS2` buffer into argv.
///
/// Layout: a 4-byte argc, the executable path, NUL padding, then `argc`
/// NUL-terminated arguments (the environment follows, and is ignored).
/// The environment a process was started with, from the same buffer as argv.
///
/// `KERN_PROCARGS2` lays out argc, the executable path, argv, then envp, so the
/// environment costs nothing beyond the read already being made. It matters
/// because argv alone does not say how a service runs: `node server.mjs` is
/// the development server or the production one depending only on `NODE_ENV`,
/// and starting the wrong one replaces a project's production build with a
/// development one it cannot boot from.
fn parse_procenv(buffer: &[u8]) -> Vec<(String, String)> {
    if buffer.len() < 4 {
        return Vec::new();
    }
    let argc = i32::from_ne_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]).max(0) as usize;

    // Skip the executable path and its padding, then argv.
    let mut position = 4;
    while position < buffer.len() && buffer[position] != 0 {
        position += 1;
    }
    while position < buffer.len() && buffer[position] == 0 {
        position += 1;
    }
    for _ in 0..argc {
        while position < buffer.len() && buffer[position] != 0 {
            position += 1;
        }
        position += 1;
    }

    let mut out = Vec::new();
    while position < buffer.len() {
        let start = position;
        while position < buffer.len() && buffer[position] != 0 {
            position += 1;
        }
        if start == position {
            // The empty string that ends envp.
            break;
        }
        let entry = String::from_utf8_lossy(&buffer[start..position]).to_string();
        if let Some((key, value)) = entry.split_once('=') {
            out.push((key.to_string(), value.to_string()));
        }
        position += 1;
    }
    out
}

fn parse_procargs(buffer: &[u8]) -> Vec<String> {
    if buffer.len() < 4 {
        return Vec::new();
    }
    let argc = i32::from_ne_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]).max(0) as usize;

    let mut position = 4;
    while position < buffer.len() && buffer[position] != 0 {
        position += 1;
    }
    while position < buffer.len() && buffer[position] == 0 {
        position += 1;
    }

    let mut args = Vec::with_capacity(argc);
    for _ in 0..argc {
        if position >= buffer.len() {
            break;
        }
        let start = position;
        while position < buffer.len() && buffer[position] != 0 {
            position += 1;
        }
        args.push(String::from_utf8_lossy(&buffer[start..position]).to_string());
        position += 1;
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_environment_that_follows_argv() {
        // The case this exists for: argv is identical between a project's dev
        // and production servers, and only NODE_ENV tells them apart.
        let mut buffer = 2i32.to_ne_bytes().to_vec();
        buffer.extend_from_slice(b"/usr/local/bin/node\0");
        buffer.extend_from_slice(b"\0\0");
        buffer.extend_from_slice(b"node\0");
        buffer.extend_from_slice(b"server.mjs\0");
        buffer.extend_from_slice(b"NODE_ENV=production\0");
        buffer.extend_from_slice(b"DATABASE_URL=postgres://secret\0");

        let env = parse_procenv(&buffer);
        assert!(env.contains(&("NODE_ENV".to_string(), "production".to_string())));
        // Everything is parsed here; the filtering to mode switches happens at
        // the provider boundary, which is what keeps credentials out of the
        // registry.
        assert_eq!(env.len(), 2);
    }

    #[test]
    fn an_environment_with_no_entries_is_not_a_panic() {
        let mut buffer = 1i32.to_ne_bytes().to_vec();
        buffer.extend_from_slice(b"/bin/ls\0\0");
        buffer.extend_from_slice(b"ls\0");
        assert!(parse_procenv(&buffer).is_empty());
    }

    #[test]
    fn parses_a_procargs_buffer() {
        let mut buffer = 2i32.to_ne_bytes().to_vec();
        buffer.extend_from_slice(b"/usr/local/bin/node\0");
        buffer.extend_from_slice(b"\0\0");
        buffer.extend_from_slice(b"node\0");
        buffer.extend_from_slice(b"server.js\0");
        buffer.extend_from_slice(b"PATH=/usr/bin\0");

        assert_eq!(parse_procargs(&buffer), vec!["node", "server.js"]);
    }

    #[test]
    fn reads_this_process_argv() {
        let argv = MacProcessProvider::command_line(std::process::id())
            .expect("own argv must be readable");
        assert!(!argv.is_empty());
    }

    #[test]
    fn reads_this_process_cwd() {
        let cwd = MacProcessProvider::cwd(std::process::id()).expect("own cwd must be readable");
        assert_eq!(cwd, std::env::current_dir().unwrap().canonicalize().unwrap());
    }
}
