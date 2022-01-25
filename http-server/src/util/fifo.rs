//! Private wrapper module for [`FifoFile`]

use std::io::{self, Read};
use std::path::Path;
use std::process::{self, Command, Stdio};

/// A file-like interface for indefinitely reading from a Unix named pipe (FIFO)
///
/// Internally, this uses `tail(1)`; it already does a bunch of hard work to ensure that we don't
/// just spin on reading from the file when there aren't any readers.
pub struct FifoFile {
    tail_cmd: process::Child,
}

impl Read for FifoFile {
    /// Blocks until input is available
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.tail_cmd.stdout.as_mut().unwrap().read(buf)
    }
}

impl Drop for FifoFile {
    fn drop(&mut self) {
        // Drop all of the io handles so that `tail` exits.
        self.tail_cmd.stdout = None;

        // We should wait for the command to finish to prevent zombies (see: Child::wait)
        let _ = self.tail_cmd.wait();
    }
}

impl FifoFile {
    /// Opens the file at the given path as a named pipe, returning an object that implements
    /// `Read` in an appropriate way
    ///
    /// Any errors that may have occured will be from spawning `tail`; actual IO errors will only
    /// be visible on the first call to `read`.
    pub fn open(path: &Path) -> io::Result<Self> {
        let tail_cmd = Command::new("tail")
            .arg("-f")
            .arg(path)
            .stdout(Stdio::piped())
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        Ok(FifoFile { tail_cmd })
    }
}
