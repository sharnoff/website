//! Wrapper module for the [`markdown_to_html`] function and its associated machinery

use anyhow::{anyhow, Context, Result};
use lazy_static::lazy_static;
use pulldown_cmark::html::push_html;
use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::io::{Read, Write};
use std::net::TcpStream;

/// Converts the markdown string to HTML
pub fn markdown_to_html(md: &str) -> String {
    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS;

    // Errors aren't possible in the parser; it always falls back to some other kind of display.
    let mut html_str = String::new();
    let mut code_state = CodeState::NotStarted;

    push_html(
        &mut html_str,
        Parser::new_ext(md, options)
            .map(proper_text_dashes)
            .map(|e| code_state.map_event(e)),
    );
    html_str
}

/// Helper function to substitute in en- and em-dashes for two and three hyphens in text,
/// respectively
///
/// This requires that there be whitespace or a newline on either side of the dashes.
fn proper_text_dashes(event: Event) -> Event {
    let mut text = match event {
        Event::Text(t) => t,
        e => return e,
    };

    lazy_static! {
        /// Matcher for three hyphens ("---") in a row with whitespace on either side
        static ref TRIPLE_HYPHEN: Regex = Regex::new(r"(^| )---( |$)").unwrap();

        /// Matcher for two hyphens ("--") in a row with whitespace on either side
        static ref DOUBLE_HYPHEN: Regex = Regex::new(r"(^| )--( |$)").unwrap();
    }

    // Check for triple dashes --> em-dash:
    let mut text_cow = TRIPLE_HYPHEN.replace_all(&text, "$1\u{2014}$2");
    // double dashes --> en-dash:
    match DOUBLE_HYPHEN.replace_all(&text, "$1\u{2013}$2") {
        t @ Cow::Owned(_) => text_cow = t,
        // Do nothing; it didn't change.
        Cow::Borrowed(_) => (),
    }

    if let Cow::Owned(s) = text_cow {
        text = CowStr::Boxed(s.into_boxed_str());
    }

    Event::Text(text)
}

/// The address of the server we connect to for syntax highlighting
static HIGHLIGHT_SERVER_ADDR: &str = "localhost:8001";

#[derive(Serialize)]
struct HighlightRequest<'md> {
    language: &'md str,
    code: &'md str,
}

#[derive(Deserialize)]
enum HighlightResponse {
    #[serde(rename = "success")]
    Success(String),
    #[serde(rename = "failure")]
    Failure(String),
}

/// Simple object to group a number of `Event`s together when it's a code block
#[derive(Debug)]
enum CodeState<'md> {
    NotStarted,
    Started {
        language: Option<Cow<'md, str>>,
    },
    AwaitingEnd {
        code: Cow<'md, str>,
        language: Option<Cow<'md, str>>,
    },
}

/// Helper function to convert from `pulldown_cmark`'s own `CowStr` type to the more standard
/// `Cow<str>`.
fn cow<'s>(cmark: CowStr<'s>) -> Cow<'s, str> {
    match cmark {
        CowStr::Boxed(b) => Cow::Owned(String::from(b)),
        CowStr::Borrowed(s) => Cow::Borrowed(s),
        CowStr::Inlined(s) => Cow::Owned(s.as_ref().to_owned()),
    }
}

impl<'md> CodeState<'md> {
    /// Extracts and processes a series of code block events, turning them into a single `Html`
    /// event with proper syntax highlighting
    ///
    /// Internally uses [`code_block_to_html`].
    fn map_event(&mut self, event: Event<'md>) -> Event<'md> {
        // Helper function -- we can output "nothing" by returning an emtpy Html event:
        let empty_event = || Event::Html(CowStr::Borrowed(""));

        // Temporarily move out of `self` so that we can take the ownership of the values.
        let this = std::mem::replace(self, CodeState::NotStarted);

        match (this, event) {
            (CodeState::NotStarted, Event::Start(Tag::CodeBlock(kind))) => {
                let language = match kind {
                    CodeBlockKind::Fenced(l) if !l.as_ref().is_empty() => Some(cow(l)),
                    _ => None,
                };

                *self = CodeState::Started { language };
                empty_event()
            }
            (CodeState::Started { language }, Event::Text(t)) => {
                let code = cow(t);
                *self = CodeState::AwaitingEnd { code, language };
                empty_event()
            }
            (CodeState::AwaitingEnd { code, language }, Event::Text(t)) => {
                let code = Cow::Owned(code.into_owned() + t.as_ref());
                *self = CodeState::AwaitingEnd { code, language };
                empty_event()
            }
            (CodeState::AwaitingEnd { code, language }, Event::End(tag)) => {
                match tag {
                    Tag::CodeBlock(_) => (),
                    t => panic!("unexpected end tag {:?} for code block", t),
                }

                // Done. We can output an html event after highlighting
                let lang = language.as_ref().map(|cow| cow.as_ref());
                let html = code_block_to_html(code.as_ref(), lang);

                Event::Html(CowStr::Boxed(html.into_boxed_str()))
            }
            (CodeState::NotStarted, e) => e,
            (s, e) => {
                panic!("unexpected event {:?} for CodeState {:?}", e, s);
            }
        }
    }
}

/// Given a block of code (and optionally, its language), produces the HTML string corresponding to
/// highlighting the code in the language
///
/// Code blocks are formatted as:
///
/// ```html
/// <pre><code class="language-<language>">
/// ...
/// </code></pre>
/// ```
///
/// Internally, this attempts to connect to a running highlighter server. Highlighting can fail for
/// a number of reasons -- on failure, we output the code as if no language was selected.
fn code_block_to_html(code: &str, language: Option<&str>) -> String {
    let new_code = match highlight(code, language) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "Could not highlight code for language {:?}: {:#}",
                language, e
            );
            Cow::Borrowed(code)
        }
    };

    let language_class = language
        .map(|l| format!(r#" class="language-{}""#, l))
        .unwrap_or_default();

    format!("<pre><code{}>\n{}\n</code></pre>", language_class, new_code)
}

fn highlight<'md>(code: &'md str, language: Option<&str>) -> Result<Cow<'md, str>> {
    let language = match language {
        // If there is no language, then we can skip highlighting:
        None => return Ok(Cow::Borrowed(code)),
        Some(l) => l,
    };

    // Are we creating a new connection each time we encounter a code block? yes.
    // Does it _really_ matter? no.
    let mut conn = TcpStream::connect(HIGHLIGHT_SERVER_ADDR).with_context(|| {
        format!(
            "failed to connect to highlighting server at {}",
            HIGHLIGHT_SERVER_ADDR
        )
    })?;

    let req = HighlightRequest { language, code };
    let mut data = serde_json::to_vec(&req).context("failed to serialize highlighting request")?;
    // We need to write a trailing null byte for the highlight server to recognize the end of the
    // request
    data.push(b'\0');

    conn.write_all(&data)
        .and_then(|_| conn.flush())
        .context("failed to write highlighting request to server")?;

    let mut resp_str = String::new();

    let resp: HighlightResponse = conn
        .read_to_string(&mut resp_str)
        .map(|_| resp_str)
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.into()))
        .context("failed to read response from highlighting server")?;

    match resp {
        HighlightResponse::Success(new_code) => Ok(Cow::Owned(new_code)),
        HighlightResponse::Failure(err_msg) => {
            Err(anyhow!("server failed to highlight code: {}", err_msg))
        }
    }
}
