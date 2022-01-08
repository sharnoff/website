//! Handling of blog posts
//!
//! The main export is the `blog_routes` macro. The `recent_posts_context` function is also
//! supplied so that the site root can display some recent posts.

use anyhow::{anyhow, bail, Context, Result};
use arc_swap::ArcSwap;
use chrono::{offset::FixedOffset, DateTime};
use glob::glob;
use lazy_static::lazy_static;
use rocket::get;
use rocket_contrib::templates::Template;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;

use crate::util::{format_datetime, is_uri_idempotent, markdown_to_html, FormatLevel};

/// Helper macro so that mounting the routes will work correctly at the crate root
macro_rules! blog_routes {
    () => {{
        rocket::routes![crate::blog::index, crate::blog::post, crate::blog::tag]
    }};
}

/// Name of the template used for the blogs overview (at "/blog")
static INDEX_TEMPLATE_NAME: &str = "blog/index";
/// Name of the template used for individual blog posts (at "/blog/<post_name>")
static POST_TEMPLATE_NAME: &str = "blog/post";
/// Name of the template used for displaying the values in a tag (at "/blog/tag/<tag_name>")
static TAGS_TEMPLATE_NAME: &str = "blog/tag";
/// Directory that the blog posts are stored in, relative to the source root
static BLOG_POSTS_DIRECTORY: &str = "content/blog-posts";
/// Glog to match the markdown document responsible for each post
static BLOG_GLOB: &str = "*.md";

/// Minimum number of markdown bytes to include in a post sneak peek
const MIN_SNEAK_PEEK_AMOUNT: usize = 100;

lazy_static! {
    /// Global state of the blog information
    static ref STATE: ArcSwap<BlogState> = match BlogState::new() {
        Ok(s) => ArcSwap::from(Arc::new(s)),
        Err(e) => {
            eprintln!("failed to create `BlogState`: {:#}", e);
            exit(1)
        }
    };
}

/// Collects all of the necessary information about the state of the blog, causing any failures to
/// happen immediately
///
/// Any failures encountered will result in an immediate exit.
pub fn initialize() {
    lazy_static::initialize(&STATE);
}

/// Re-makes the `BlogState` to incorporate any recent file changes
pub fn update() -> Result<()> {
    // Blog stuff is relatively cheap; we can afford to just recalculate the entire state whenever
    // there's a change.
    let new_state = BlogState::new()?;

    STATE.store(Arc::new(new_state));

    Ok(())
}

#[get("/")]
pub fn index() -> Template {
    let ctx = STATE.load().index_context();
    Template::render(INDEX_TEMPLATE_NAME, ctx)
}

#[get("/<post_name>")]
pub fn post(post_name: Cow<str>) -> Option<Template> {
    assert!(!post_name.is_empty());

    let ctx = STATE.load().post_context(&*post_name)?;
    Some(Template::render(POST_TEMPLATE_NAME, ctx))
}

#[get("/tag/<tag>")]
pub fn tag(tag: String) -> Option<Template> {
    let ctx = STATE.load().tag_context(&tag)?;
    Some(Template::render(TAGS_TEMPLATE_NAME, ctx))
}

pub fn recent_posts_context() -> Vec<Arc<PostContext>> {
    STATE.load().recent_posts_context()
}

impl BlogState {
    /// Creates the `BlogState`, returning any error if applicable
    fn new() -> Result<Self> {
        let mut files = HashMap::new();

        let mut by_time = BTreeMap::new();
        let mut tags: HashMap<String, BTreeMap<_, _>> = HashMap::new();

        // Each blog post exists as a separate markdown file in the blogs directory
        let glob_pat = format!("{}/{}", BLOG_POSTS_DIRECTORY, BLOG_GLOB);
        for glob_result in glob(&glob_pat).expect("failed to read glob pattern") {
            let file_path = glob_result.context("failed to get glob item for blog posts")?;

            let file_name: PathBuf = file_path
                .file_prefix()
                .expect("expected glob result to have file name")
                .into();

            if !is_uri_idempotent(&file_name.to_string_lossy()) {
                bail!(
                    "bad entry file name {:?}: must URI encode to the same value",
                    file_path.file_name().unwrap()
                );
            }

            let info: Arc<_> = fs::read_to_string(&file_path)
                .context("could not read to string")
                .and_then(|c| PostContext::from_file_content(&file_name, &c))
                .with_context(|| format!("could not parse file {:?}", file_name))?
                .into();

            // Add info to the blog state
            let time = info.meta.published_unix_time;

            by_time.insert(time, info.clone());
            for t in &info.meta.tags {
                tags.entry(t.to_owned())
                    .or_default()
                    .insert(time, info.clone());
            }

            files.insert(file_name, info);
        }

        Ok(BlogState {
            files,
            tags,
            by_time,
        })
    }
}

impl PostContext {
    fn from_file_content(path: &Path, content: &str) -> Result<Self> {
        // Split the string into the header & body:
        //
        // The header exists until the first line that equals '+++'. So we can just directly split
        // the file
        let (header, body) = content
            .split_once("\n+++\n")
            .ok_or_else(|| anyhow!("file must include '\\n+++\\n' to split header & body"))?;

        // We just parse the top of the file as TOML
        #[derive(Deserialize)]
        struct ParsedMeta {
            title: String,
            tab_title: Option<String>,
            description: String,
            first_published: ParsedDateTime,
            updated: Vec<ParsedDateTime>,
            tags: Vec<String>,
        }

        #[derive(Deserialize)]
        #[serde(try_from = "String")]
        struct ParsedDateTime(DateTime<FixedOffset>);

        impl TryFrom<String> for ParsedDateTime {
            type Error = chrono::ParseError;

            fn try_from(s: String) -> chrono::ParseResult<Self> {
                DateTime::parse_from_rfc2822(&s).map(ParsedDateTime)
            }
        }

        let parsed: ParsedMeta = toml::from_str(header).context("failed to parse header")?;

        // Figure out how much to show as a sneak peek for the blog post. We *could* do this
        // semantically by the parsed markdown, but directly going off of the byte sizes is just
        // easier.
        //
        // Essentially what we're doing is getting enough paragraphs of input so that there's at
        // least MIN_SNEAK_PEEK_AMOUNT bytes of raw markdown represented.
        let sneak_peek_amount = body
            // Double newline signifies a new paragraph -- usually.
            .matches("\n\n")
            .map(|m| m.as_ptr() as usize - body.as_ptr() as usize)
            .find(|a| a >= &MIN_SNEAK_PEEK_AMOUNT)
            .unwrap_or_else(|| body.len());

        let tab_title = parsed.tab_title.unwrap_or_else(|| parsed.title.clone());
        let meta = PostMeta {
            path: path.to_owned(),
            title: parsed.title,
            tab_title,
            sneak_peek: markdown_to_html(&body[..sneak_peek_amount]),
            description: markdown_to_html(&parsed.description),
            first_published: format_datetime(parsed.first_published.0, FormatLevel::Date),
            updated: parsed
                .updated
                .into_iter()
                .map(|d| format_datetime(d.0, FormatLevel::Date))
                .collect(),
            tags: parsed.tags,
            published_unix_time: parsed.first_published.0.timestamp(),
        };

        Ok(PostContext {
            meta,
            html_body_content: markdown_to_html(body),
        })
    }
}

/// The total stored state of the blog, a single instance of which is stored in `STATE`
#[derive(Debug)]
struct BlogState {
    /// Mapping of file / directory names
    files: HashMap<PathBuf, Arc<PostContext>>,
    /// All of the tags and the posts
    tags: HashMap<String, BTreeMap<i64, Arc<PostContext>>>,
    /// Entry names, sorted by their publishing timestamp
    by_time: BTreeMap<i64, Arc<PostContext>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PostContext {
    meta: PostMeta,
    /// The body of the blog post, as HTML
    html_body_content: String,
}

#[derive(Debug, Clone, Serialize)]
struct PostMeta {
    /// The path to the post
    path: PathBuf,
    /// The name of the blog post displayed at the top of the page
    title: String,
    /// The name used for titling the tab. Defaults to `title` if not given
    tab_title: String,
    /// HTML string of the first few bits of content from the post
    sneak_peek: String,
    /// Description / subtitle of the post, as HTML
    description: String,
    /// Pretty-printed date/time at which the post was first published
    first_published: String,
    /// All of the times at which the post was updated
    updated: Vec<String>,
    /// Tags associated with the post
    tags: Vec<String>,
    /// The "first published" timestamp, represented as seconds since the Unix epoch. Stored for
    /// sorting.
    published_unix_time: i64,
}

#[derive(Debug, Clone, Serialize)]
struct IndexContext {
    posts: Vec<Arc<PostContext>>,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct TagContext {
    tag: String,
    posts: Vec<Arc<PostContext>>,
}

impl BlogState {
    fn index_context(&self) -> IndexContext {
        IndexContext {
            tags: self.tags.keys().cloned().collect(),
            posts: self.by_time.iter().map(|(_, i)| i).cloned().rev().collect(),
        }
    }

    fn post_context(&self, name: impl AsRef<Path>) -> Option<Arc<PostContext>> {
        self.files.get(name.as_ref()).cloned()
    }

    fn tag_context(&self, name: &str) -> Option<TagContext> {
        Some(TagContext {
            tag: name.to_owned(),
            posts: self.tags.get(name)?.values().cloned().rev().collect(),
        })
    }

    fn recent_posts_context(&self) -> Vec<Arc<PostContext>> {
        self.by_time.values().cloned().rev().collect()
    }
}
