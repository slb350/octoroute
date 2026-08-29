//! Cancellation-safe ownership for one spawned process tree.

#[cfg(unix)]
mod imp {
    use std::{io, os::unix::process::CommandExt as _};
    use tokio::process::{Child, Command};

    pub(crate) fn isolate(command: &mut Command) {
        command.as_std_mut().process_group(0);
    }

    /// Own the process group led by one spawned child.
    ///
    /// Dropping the future that owns this guard still kills the whole group.
    /// Normal timeout paths use [`ProcessGroup::terminate`] so signal and reap
    /// errors remain observable.
    pub(crate) struct ProcessGroup {
        pgid: Option<i32>,
    }

    impl ProcessGroup {
        pub(crate) fn for_child(child: &Child) -> io::Result<Self> {
            let pid = child
                .id()
                .and_then(|pid| i32::try_from(pid).ok())
                .ok_or_else(|| io::Error::other("spawned child has no valid process id"))?;
            Ok(Self { pgid: Some(pid) })
        }

        pub(crate) async fn terminate(&mut self, child: &mut Child) -> io::Result<()> {
            let signal = self.signal(libc::SIGKILL);
            let wait = child.wait().await.map(|_| ());
            let result = signal.and(wait);
            if result.is_ok() {
                self.pgid = None;
            }
            result
        }

        fn signal(&self, signal: i32) -> io::Result<()> {
            let Some(pgid) = self.pgid else {
                return Ok(());
            };
            // SAFETY: `pgid` came from the successfully spawned group leader
            // and is negated only to select its Unix process group.
            let result = unsafe { libc::kill(-pgid, signal) };
            if result == 0 {
                Ok(())
            } else {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    impl Drop for ProcessGroup {
        fn drop(&mut self) {
            // Drop cannot report an error. It is the cancellation backstop;
            // ordinary error and timeout paths call `terminate` and propagate.
            let _ = self.signal(libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use std::io;
    use tokio::process::{Child, Command};

    pub(crate) fn isolate(_command: &mut Command) {}

    pub(crate) struct ProcessGroup;

    impl ProcessGroup {
        pub(crate) fn for_child(child: &Child) -> io::Result<Self> {
            child
                .id()
                .map(|_| Self)
                .ok_or_else(|| io::Error::other("spawned child has no process id"))
        }

        pub(crate) async fn terminate(&mut self, child: &mut Child) -> io::Result<()> {
            child.kill().await?;
            child.wait().await.map(|_| ())
        }
    }
}

pub(super) use imp::{ProcessGroup, isolate};

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::Command;

    #[tokio::test]
    async fn termination_kills_a_spawned_grandchild_too() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 30 & echo $!; wait"])
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        isolate(&mut command);
        let mut child = command.spawn().expect("spawn group leader");
        let mut process_group = ProcessGroup::for_child(&child).expect("process group");
        let stdout = child.stdout.take().expect("child stdout");
        let mut lines = BufReader::new(stdout).lines();
        let grandchild_pid: i32 = lines
            .next_line()
            .await
            .expect("read grandchild pid")
            .expect("grandchild pid line")
            .parse()
            .expect("numeric grandchild pid");

        process_group
            .terminate(&mut child)
            .await
            .expect("terminate process group");

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            // SAFETY: signal zero performs an existence check and does not
            // alter the process.
            let exists = unsafe { libc::kill(grandchild_pid, 0) } == 0;
            if !exists {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "grandchild {grandchild_pid} survived group cleanup"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
