//! Crate-wide utilities

use chrono::{DateTime, FixedOffset};
use rocket::response::{self, Responder};
use rocket::{http, Request};
use std::ffi::CString;
use std::io::{self, Read};
use std::ops::RangeInclusive;
use std::os::unix::{ffi::OsStrExt, io::RawFd};
use std::path::Path;

/// The character ranges that get mapped to the same value when URI encoded
///
/// These form the set of allowed characters in a number of different contexts; e.g. blog post
/// paths, album names, etc -- so that the URLs to access them remain pretty simple
///
/// See: RFC 3986, https://en.wikipedia.org/wiki/Query_string#URL_encoding
static URI_ENCODE_AS_IS_RANGES: &[RangeInclusive<char>] = &[
    // Ranges A-Z, a-z, 0-9
    'A'..='Z',
    'a'..='z',
    '0'..='9',
    // Individual characters '-', '~', '.', '_'
    '-'..='-',
    '~'..='~',
    '.'..='.',
    '_'..='_',
];

/// Returns false if the string has any characters that aren't URI encoded to themselves
pub fn is_uri_idempotent(s: &str) -> bool {
    s.chars()
        .all(|c| URI_ENCODE_AS_IS_RANGES.iter().any(|r| r.contains(&c)))
}

/// Selector for which `DateTime` formatter to use
pub enum FormatLevel {
    /// Mon(th) Day, Year; e.g. "Nov 7, 2021"
    Date,
    /// Hour:Minute:Second Mon(th) Day Year Offset; e.g. "13:27:45 Nov 07 2021 -08:00"
    DateTime,
    /// Hour:Minute:Second Offset; e.g. "13:27:45"
    LocalTime,
    /// Offset; e.g. "-08:00"
    Offset,
}

/// Standard formatting for the provided `DateTime`, given the level of detail with which to format
pub fn format_datetime(datetime: DateTime<FixedOffset>, selector: FormatLevel) -> String {
    let fmt_str = match selector {
        FormatLevel::Date => "%b %-d, %Y",
        FormatLevel::DateTime => "%H:%M:%S %b %d %Y %Z",
        FormatLevel::LocalTime => "%H:%M:%S",
        FormatLevel::Offset => "%Z",
    };

    datetime.format(fmt_str).to_string()
}

/// Converts the markdown string to HTML
pub fn markdown_to_html(md: &str) -> String {
    use pulldown_cmark::html::push_html;
    use pulldown_cmark::{Options, Parser};

    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS;

    // Errors aren't possible in the parser; it always falls back to some other kind of display.
    let mut html_str = String::new();
    push_html(&mut html_str, Parser::new_ext(md, options));
    html_str
}

/// Wrapper between a responder `R` and a possible indication that the requested URL has been
/// permanently moved
///
/// This is particularly useful for things like serving images, which get an updated hash whenever
/// the content of the image changes.
pub enum MaybeRedirect<R> {
    Dont(R),
    Redirect {
        new_url: http::uri::Origin<'static>,
        is_permanent: bool,
    },
}

impl<'r, R> Responder<'r> for MaybeRedirect<R>
where
    R: Responder<'r>,
{
    fn respond_to(self, req: &Request) -> response::Result<'r> {
        use response::Redirect;

        match self {
            Self::Dont(r) => r.respond_to(req),
            Self::Redirect {
                new_url,
                is_permanent: false,
            } => Redirect::to(new_url).respond_to(req),
            Self::Redirect {
                new_url,
                is_permanent: true,
            } => Redirect::permanent(new_url).respond_to(req),
        }
    }
}

/// A file-like interface for reading from a Unix named pipe (FIFO)
///
/// The implementation of `Read` blocks until there is input available.
pub struct FifoFile {
    fd: RawFd,
}

impl Read for FifoFile {
    /// Blocks until input is available
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // There's a couple of annoying things here.
        //
        // You might think that we can just use blocking reads, but it's not that simple. In fact,
        // the reason why we can't use `fs::File` in the first place: once there are no writers for
        // a pipe, calls to `read()` will always return `Ok(0)` - i.e. no bytes read.
        //
        // Note: this isn't EOF -- other writers can still attach. So the naive solution would
        // spin, producing continual zero-length reads.
        //
        // ---
        //
        // The solution is to then use something like `poll`, which allows us to block until the
        // pipe has more data inside of it.
        //
        // So the general plan is: read until we get nothing back (or: would block), then wait for
        // another writer to give us more.

        // Read in a loop because it's theoretically possible to get spurious wakes from poll.
        // There's a 'BUGS' section in select(2) that goes over some sources. Realistically, we
        // should be fine, but it's probably better to be safe.
        loop {
            unsafe {
                // Try reading:
                let buf_ptr = buf.as_mut_ptr() as *mut libc::c_void;
                match libc::read(self.fd, buf_ptr, buf.len()) {
                    0 => (), // wait for input
                    -1 => {
                        let err = io::Error::last_os_error();

                        // We initialized the pipe in non-blocking mode; we need to handle
                        // EWOULDBLOCK errors that just mean we should wait.
                        match err.kind() {
                            io::ErrorKind::WouldBlock => (), // wait for input
                            _ => return Err(err),
                        }
                    }
                    // Success - return that bytes were read.
                    n => {
                        assert!(n > 0);
                        return Ok(n as usize);
                    }
                }

                // There isn't anything in the fifo currently; wait for more:
                let mut poll = libc::pollfd {
                    fd: self.fd,
                    events: libc::POLLIN,
                    revents: 0_i16,
                };

                // poll expects an "array"; we only have a single file descriptor, so we can pass
                // it as-is -- but we need to indicate that it's just the one.
                let nfds = 1;

                // Per poll(2):
                //
                // > Specifying a negative value in timeout means an infinite timeout.
                let timeout = -1;

                if libc::poll(&mut poll, nfds, timeout) == -1 {
                    return Err(io::Error::last_os_error());
                }

                // If no errors, we're ready to read again!
            }
        }
    }
}

impl Drop for FifoFile {
    fn drop(&mut self) {
        // No point in checking for errors; we don't have a reasonable way to report it if there is
        // one.
        let _ = unsafe { libc::close(self.fd) };
    }
}

impl FifoFile {
    /// Opens the file at the given path as a named pipe, returning an object that implements
    /// `Read` in an appropriate way
    ///
    /// Returns errors if the file cannot be opened or if it is not a pipe.
    pub fn open(path: &Path) -> io::Result<Self> {
        use std::mem::MaybeUninit;

        // Taken from the standard library, fn cstr in std/src/sys/unix/fs.rs
        let c_path = CString::new(path.as_os_str().as_bytes())?;

        // Non-blocking so that opening the fifo doesn't have to wait for the writing end to connect.
        //
        // > Normally, opening the FIFO blocks until the other end is opened also.
        // >
        // > A process can open a FIFO in nonblocking mode. In this case, opening for read-only
        // > succeeds even if no one has opened on the write side yet ...
        //
        // See man 7 fifo.
        let flags = libc::O_NONBLOCK
            // man 7 pipe:
            //
            // > A FIFO ... is opened using open(2). ... The read end is opened using the O_RDONLY flag
            | libc::O_RDONLY;

        // The mode doesn't matter, because we aren't going to create the file if it isn't there.
        let mode: libc::c_int = 0;

        // Get the file descriptor
        let fd = unsafe {
            match libc::open(c_path.as_ptr(), flags, mode) {
                -1 => return Err(io::Error::last_os_error()),
                fd => fd,
            }
        };

        // Get file information (`stat`)
        let stat = unsafe {
            let mut s = MaybeUninit::uninit();

            match libc::fstat(fd, s.as_mut_ptr()) {
                -1 => return Err(io::Error::last_os_error()),
                0 => (),
                n => {
                    eprintln!("unexpected return {} from fstat, aborting.", n);
                    std::process::abort();
                }
            }

            // Because `fstat` was successful, the stat pointer will have been initialized.
            s.assume_init()
        };

        // Check that the file *is* actually a pipe.
        //
        // There's a great discussion of this in man 7 inode, under "The file type and mode".
        if (libc::S_IFMT & stat.st_mode) != libc::S_IFIFO {
            return Err(io::Error::new(io::ErrorKind::Other, "file is not a FIFO"));
        }

        Ok(FifoFile { fd })
    }
}
