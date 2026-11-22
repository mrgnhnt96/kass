//! Runs the bundled server the way Tauri's shell plugin did: output comes
//! back a line at a time, and its stdin stays open until Kass lets go of it.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{ChildStdin, Stdio};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

pub enum CommandEvent {
    /// A line of the process's output, without its line break.
    Stdout(Vec<u8>),
    /// A line of the process's error output, without its line break.
    Stderr(Vec<u8>),
    /// The process has exited.
    Terminated,
}

pub struct Command(std::process::Command);

impl Command {
    pub fn new(program: impl AsRef<Path>) -> Self {
        Self(std::process::Command::new(program.as_ref()))
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.0.args(args);
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.0.env(key, value);
        self
    }

    /// Start the process. The channel closes once its output has ended and
    /// it has exited.
    pub fn spawn(mut self) -> std::io::Result<(UnboundedReceiver<CommandEvent>, Child)> {
        let mut process = self
            .0
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let (tx, rx) = unbounded_channel();
        if let Some(out) = process.stdout.take() {
            read_lines(out, tx.clone(), CommandEvent::Stdout);
        }
        if let Some(err) = process.stderr.take() {
            read_lines(err, tx.clone(), CommandEvent::Stderr);
        }
        let child = Child {
            pid: process.id(),
            _stdin: process.stdin.take(),
        };
        // Reap it when it ends, so it doesn't linger as a zombie.
        std::thread::spawn(move || {
            let _ = process.wait();
            let _ = tx.send(CommandEvent::Terminated);
        });
        Ok((rx, child))
    }
}

fn read_lines<R: Read + Send + 'static>(
    stream: R,
    tx: UnboundedSender<CommandEvent>,
    wrap: fn(Vec<u8>) -> CommandEvent,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                        line.pop();
                    }
                    // Keep draining when nobody listens, so the process
                    // never blocks on a full pipe.
                    let _ = tx.send(wrap(line));
                }
            }
        }
    });
}

/// A started process. Dropping it closes the process's stdin.
pub struct Child {
    pid: u32,
    _stdin: Option<ChildStdin>,
}

impl Child {
    pub fn pid(&self) -> u32 {
        self.pid
    }
}
