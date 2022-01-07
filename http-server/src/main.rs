#![feature(proc_macro_hygiene, decl_macro, path_file_prefix)]

use rocket::response::NamedFile;
use rocket::{get, http, routes};
use rocket_contrib::templates::Template;
use serde::Serialize;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[macro_use] // <- gives us `blog_routes!`
mod blog;
#[macro_use] // <- gives us `photos_routes!`
mod photos;
mod util;

fn main() {
    let rocket = rocket::ignite()
        .mount("/blog", blog_routes!())
        .mount("/photos", photos_routes!())
        .mount("/", routes![index, static_asset])
        .attach(Template::fairing());

    blog::initialize();
    photos::initialize();

    rocket.launch();
}

/// Name of the local directory used to store static content at the site root
static STATIC_DIRNAME: &str = "static";
/// Name of the template used for the site root
static INDEX_TEMPLATE_NAME: &str = "index";

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
