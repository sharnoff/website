//! Crate-wide utilities

use chrono::{DateTime, FixedOffset};
use rocket::response::{self, Responder};
use rocket::{http, Request};
use std::ops::RangeInclusive;

mod fifo;
mod html;

pub use fifo::FifoFile;
pub use html::markdown_to_html;

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
