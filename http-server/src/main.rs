#![feature(proc_macro_hygiene, decl_macro, path_file_prefix)]

#[cfg(not(target_os = "linux"))]
compile_error!("this server makes assumptions that may only be true on Linux");

use anyhow::{anyhow, Context};
use chrono::{SecondsFormat, Utc};
use rocket::response::NamedFile;
use rocket::{get, http, routes};
use rocket_contrib::templates::Template;
use serde::Serialize;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[macro_use] // <- gives us `blog_routes!`
mod blog;
#[macro_use] // <- gives us `photos_routes!`
mod photos;
mod util;

use util::FifoFile;

fn main() {
    let rocket = rocket::ignite()
        .mount("/blog", blog_routes!())
        .mount("/photos", photos_routes!())
        .mount("/", routes![index, static_asset])
        .attach(Template::fairing());

    if cfg!(not(debug_assertions)) {
        blog::initialize();
        photos::initialize();
    }

    let updates_path_result = fs::canonicalize(UPDATE_PIPE_PATH)
        .with_context(|| format!("failed to canonicalize updates path {:?}", UPDATE_PIPE_PATH));

    let updates_path = match updates_path_result {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{:#}", e);
            exit(1);
        }
    };

    thread::spawn(move || listen_for_updates(&updates_path));

    rocket.launch();
}

/// Name of the local directory used to store static content at the site root
static STATIC_DIRNAME: &str = "static";
/// Name of the template used for the site root
static INDEX_TEMPLATE_NAME: &str = "index";
/// Filename of the pipe to listen to for updates to the site content
static UPDATE_PIPE_PATH: &str = "updated";
/// Time to wait if we can't open the updates pipe; 5 minutes.
const UPDATE_RETRY_WAIT_DURATION: Duration = Duration::from_secs(300);

/// Template context for the site root
#[derive(Serialize)]
struct IndexContext {
    /// List of post contexts, supplied by `crate::blog`
    posts: Vec<Arc<blog::PostContext>>,

    /// List of photo contexts, supplied by `crate::photos`
    photos: Vec<Arc<photos::PhotoInfo>>,

    flex_grid_settings: photos::FlexGridSettings,
}

#[get("/")]
fn index() -> Template {
    let ctx = IndexContext {
        posts: blog::recent_posts_context(),
        photos: photos::recent_photos_context(),
        flex_grid_settings: photos::FlexGridSettings {
            ..Default::default()
        },
    };

    Template::render(INDEX_TEMPLATE_NAME, ctx)
}

// Static assets are *accessed* as if they're in the root directory, but they're actually all
// stored in the 'static' subdirectory. We have them over there just to keep things clean :)
//
// Rocket incorrectly classifies the rank of this route, so we have to reduce its precedence a bit
// extra (hence rank = 0)
#[get("/<file_path..>", rank = 0)]
fn static_asset(file_path: PathBuf) -> Result<NamedFile, http::Status> {
    // Rocket's implementation of FromSegments for PathBuf ensures that we don't end up with paths
    // leading outside of the original directory -- i.e. it protects against path traversal
    // attacks.
    //
    //   per the Rocket docs: https://rocket.rs/v0.5-rc/guide/requests/#multiple-segments
    NamedFile::open(Path::new(STATIC_DIRNAME).join(file_path)).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => http::Status::NotFound,
        _ => http::Status::InternalServerError,
    })
}

/// On each successful read of `UPDATE_PIPE_PATH`, calls the update functions for the relevant
/// components of the server
///
/// On a failed read, attempts to re-open the file. If the file cannot be opened, it will retry
/// every `UPDATE_RETRY_WAIT_DURATION` and log an error each time it fails.
fn listen_for_updates(canonical_path: &Path) -> ! {
    // Helper function to format the current time
    let get_time = || Utc::now().to_rfc3339_opts(SecondsFormat::Millis, false);

    loop {
        // Try to get the file
        let file = loop {
            match FifoFile::open(canonical_path) {
                Ok(f) => break f,
                Err(e) => eprintln!("ERROR @ {} :: {}", get_time(), e),
            }

            // Wait to retry.
            thread::sleep(UPDATE_RETRY_WAIT_DURATION);
        };

        let mut reader = BufReader::new(file);

        loop {
            let mut buf = String::new();
            let result = reader.read_line(&mut buf).with_context(|| {
                format!("failed to read from update pipe at {:?}", canonical_path)
            });

            if let Err(e) = result {
                eprintln!("ERROR @ {} :: {:#}", get_time(), e);
                break; // Go back and try to re-open the file
            }

            println!("INFO @ {} :: received update request {:?}", get_time(), buf);

            for component in buf.trim().split(' ') {
                let func = match component {
                    "photos" => photos::update,
                    "blog" => blog::update,
                    s => {
                        let err = anyhow!("skipping unrecognized update component {:?}", s);
                        eprintln!("ERROR @ {} :: {:#}", get_time(), err);
                        continue;
                    }
                };

                let result =
                    func().with_context(|| format!("failed to update component {:?}", component));

                if let Err(e) = result {
                    eprintln!("ERROR @ {} :: {:#}", get_time(), e);
                } else {
                    println!("INFO @ {} :: updated component {:?}", get_time(), component);
                }
            }

            println!("INFO @ {} :: update complete", get_time());
        }
    }
}
