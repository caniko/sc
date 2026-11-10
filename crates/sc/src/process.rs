//! Bounded child processes; their entire process group is terminated on exit.
use anyhow::{ensure, Context, Result};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Process(pub Child, bool);

impl Process {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        command.process_group(0);
        let parent = unsafe { libc::getpid() };
        // SAFETY: only async-signal-safe syscalls run between fork and exec.
        // A sudden supervisor death cannot leave its direct workload running.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::raise(libc::SIGKILL);
                }
                Ok(())
            });
        }
        Ok(Self(
            command.spawn().context("start managed child process")?,
            false,
        ))
    }

    pub fn has_exited(&self) -> Result<bool> {
        if self.1 {
            return Ok(true);
        }
        // WNOWAIT retains the zombie/PID until group cleanup is complete.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.0.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        ensure!(
            result == 0,
            "inspect managed child: {}",
            std::io::Error::last_os_error()
        );
        Ok(unsafe { info.si_pid() } != 0)
    }

    pub fn stop(&mut self) -> Result<()> {
        if self.1 {
            return Ok(());
        }
        let group = i32::try_from(self.0.id()).context("child PID outside pid_t")?;
        // SAFETY: process_group(0) created this dedicated group. No unrelated
        // process is targeted; the unreaped child keeps the group ID pinned.
        unsafe {
            libc::kill(-group, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.has_exited()? {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
        self.0.wait().context("reap managed child")?;
        self.1 = true;
        Ok(())
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("child cleanup failed: {error:#}");
        }
    }
}

/// Small trusted telemetry commands only. Drain stdout concurrently so a
/// broken executable cannot block the watchdog by filling its pipe.
pub fn output(command: &mut Command, timeout: Duration) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut process = Process::spawn(command.stdout(Stdio::piped()).stderr(Stdio::null()))?;
    let mut stdout = process
        .0
        .stdout
        .take()
        .context("child stdout unavailable")?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .by_ref()
            .take(65537)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = send.send(result);
    });
    let deadline = Instant::now() + timeout;
    loop {
        if process.has_exited()? {
            break;
        }
        ensure!(Instant::now() < deadline, "telemetry command timed out");
        std::thread::sleep(Duration::from_millis(25));
    }
    process.stop()?;
    let status = process.0.wait()?;
    ensure!(status.success(), "telemetry command failed: {status}");
    let bytes = receive
        .recv_timeout(Duration::from_millis(100))
        .context("telemetry pipe did not close after child exit")??;
    ensure!(bytes.len() <= 65536, "oversized telemetry response");
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn telemetry_timeout_terminates_and_reaps_the_child() {
        let start = Instant::now();
        let result = output(Command::new("sleep").arg("30"), Duration::from_millis(100));
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(4));
    }
    #[test]
    fn bounded_output_preserves_success_and_failure_status() {
        let bytes = output(
            Command::new("printf").arg("telemetry"),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(bytes, b"telemetry");
        assert!(output(&mut Command::new("false"), Duration::from_secs(1)).is_err());
    }
    #[test]
    fn cleanup_kills_descendants_even_after_the_process_group_leader_exits() {
        use std::io::{BufRead, BufReader};
        let mut process = Process::spawn(
            Command::new("sh")
                .args(["-c", "sleep 30 & echo $!"])
                .stdout(Stdio::piped()),
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(process.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let descendant: u32 = line.trim().parse().unwrap();
        process.stop().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let state = std::fs::read_to_string(format!("/proc/{descendant}/stat"));
            if !state.as_ref().is_ok_and(|s| !s.contains(") Z ")) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "managed descendant survived group cleanup"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
