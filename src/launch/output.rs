use std::{process::ExitStatus, sync::Arc};

use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    task::JoinHandle,
};

use crate::error::Result;

/// Which stream a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Stdout,
    Stderr,
}

/// One line of game output, with its source stream.
#[derive(Debug, Clone)]
pub struct OutputLine {
    pub kind: OutputKind,
    pub line: String,
}

impl OutputLine {
    pub fn is_stderr(&self) -> bool {
        self.kind == OutputKind::Stderr
    }
}

/// Output callback. `Arc` so it can be cloned into the two reader tasks
/// (stdout / stderr) and outlive the caller's stack frame.
///
/// The callback runs on a tokio task, so it must not block for long; push the
/// line into a channel / log if any real work is needed.
pub type OutputFn = Arc<dyn Fn(OutputLine) + Send + Sync + 'static>;

pub fn no_output() -> OutputFn {
    Arc::new(|_| {})
}

/// A running game process whose stdout/stderr are being piped to an `OutputFn`.
///
/// Dropping this does *not* kill the game (same as `std::process::Child`); the
/// reader tasks end on their own when the pipes close.
pub struct GameProcess {
    child: tokio::process::Child,
    /// Cached: `Child::id()` returns None once the process has been waited on.
    pid: Option<u32>,
    readers: Vec<JoinHandle<()>>,
}

impl GameProcess {
    /// Take the piped stdout/stderr and start pumping lines into `sink`.
    pub(crate) fn new(mut child: tokio::process::Child, sink: OutputFn) -> Self {
        let pid = child.id();
        let mut readers = Vec::new();

        if let Some(stdout) = child.stdout.take() {
            readers.push(spawn_pump(stdout, OutputKind::Stdout, sink.clone()));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(spawn_pump(stderr, OutputKind::Stderr, sink));
        }

        Self { child, pid, readers }
    }

    /// PID of the game process (None once it has exited).
    pub fn id(&self) -> Option<u32> {
        self.pid
    }

    /// Wait for the game to exit, then for every pending output line to be
    /// delivered — after this returns, the callback will not fire again.
    pub async fn wait(&mut self) -> Result<ExitStatus> {
        let status = self.child.wait().await?;
        for reader in self.readers.drain(..) {
            let _ = reader.await;
        }
        Ok(status)
    }

    /// Check whether the game has exited, without blocking.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    /// Kill the game and reap it.
    pub async fn kill(&mut self) -> Result<()> {
        self.child.kill().await?;
        Ok(())
    }

    /// Escape hatch for anything not covered above (e.g. `start_kill`).
    pub fn child_mut(&mut self) -> &mut tokio::process::Child {
        &mut self.child
    }
}

/// Read `reader` line by line into `sink` until EOF.
///
/// Uses `read_until` + `from_utf8_lossy` rather than `lines()`: crash reports and
/// some mods emit non-UTF-8 bytes, which would abort a `lines()` stream.
fn spawn_pump<R>(reader: R, kind: OutputKind, sink: OutputFn) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buf = BufReader::new(reader);
        let mut bytes = Vec::new();

        loop {
            bytes.clear();
            match buf.read_until(b'\n', &mut bytes).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    while matches!(bytes.last(), Some(b'\n') | Some(b'\r')) {
                        bytes.pop();
                    }
                    let line = String::from_utf8_lossy(&bytes).into_owned();
                    sink(OutputLine { kind, line });
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[tokio::test]
    async fn pump_splits_lines_and_strips_crlf() {
        let data: &[u8] = b"first\r\nsecond\nthird-no-newline";
        let collected = Arc::new(Mutex::new(Vec::new()));
        let sink_store = collected.clone();
        let sink: OutputFn = Arc::new(move |l: OutputLine| {
            sink_store.lock().unwrap().push(l.line);
        });

        spawn_pump(data, OutputKind::Stdout, sink).await.unwrap();

        let lines = collected.lock().unwrap().clone();
        assert_eq!(lines, vec!["first", "second", "third-no-newline"]);
    }

    #[tokio::test]
    async fn pump_survives_invalid_utf8() {
        let data: &[u8] = b"ok\n\xff\xfe bad\n";
        let collected = Arc::new(Mutex::new(Vec::new()));
        let sink_store = collected.clone();
        let sink: OutputFn = Arc::new(move |l: OutputLine| {
            sink_store.lock().unwrap().push(l.line);
        });

        spawn_pump(data, OutputKind::Stderr, sink).await.unwrap();

        let lines = collected.lock().unwrap().clone();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "ok");
        assert!(lines[1].ends_with(" bad"));
    }
}
