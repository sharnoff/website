//! Handling of photos
//!
//! The main export is the `photos_routes` macro. The `recent_photos_context` function is also
//! supplied so that the site root can display some recent photos.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{Date, DateTime, FixedOffset, TimeZone};
use glob::glob;
use lazy_static::lazy_static;
use rayon::prelude::*;
use rocket::response::{self, NamedFile, Responder};
use rocket::{get, http, uri, Request};
use rocket_contrib::templates::Template;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::fmt::{self, Debug, Formatter};
use std::fs;
use std::io::{self, Cursor, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread;

use crate::util::{
    format_datetime, is_uri_idempotent, markdown_to_html, FormatLevel, MaybeRedirect,
};

/// Helper macro so that mounting the routes will work correctly at the crate root
macro_rules! photos_routes {
    () => {{
        rocket::routes![
            crate::photos::index,
            crate::photos::albums,
            crate::photos::img_page,
            crate::photos::album_page,
            crate::photos::img,
            crate::photos::map,
        ]
    }};
}

/// Name of the template used for the photos index page (at "/photos")
static INDEX_TEMPLATE_NAME: &str = "photos/index";
/// Name of the template used for displaying *all* the albums (at "/albums")
///
/// Not to be confused with the singular `ALBUMS_TEMPLATE_NAME`
static ALBUMS_TEMPLATE_NAME: &str = "photos/albums";
/// Name of the template used for displaying individual photos (at "/photos/<name>")
static IMG_TEMPLATE_NAME: &str = "photos/photo";
/// Name of the template used for albums (at "/photos/album/<name>")
static ALBUM_TEMPLATE_NAME: &str = "photos/album";
/// Name of the template used for the page containing a map of every image with a location
static MAP_TEMPLATE_NAME: &str = "photos/map";

/// Directory that images (+ album lists, metadata) are stored in
static IMGS_DIRECTORY: &str = "content/photos";
/// Pattern inside `IMGS_DIRECTORY` to match each individual photo
static IMGS_GLOB: &str = "*.jpg";
/// The extension used for "full" images, stored on disk
static FULL_IMG_EXT: &str = "jpg";
/// File name inside `IMGS_DIRECTORY` that the meta information about albums is stored at
static ALBUMS_META_FILENAME: &str = "albums.json";
/// File name inside `IMGS_DIRECTORY` in which the default configuration for `FlexGrid` is stored
static FLEXGRID_SETTINGS_FILENAME: &str = "default-flex-grid-config.json";

/// The prefix on the first line of the description used to indicate it's providing the alt text of
/// the image
///
/// For now, we require that *only* the first line is used for the alt text; so line-wrapping must
/// be ignored on that line.
static ALT_TEXT_PREFIX: &str = "alt:";

/// Number of photos to show at the site root, as a preview
const NUM_PREVIEW_PHOTOS: usize = 5;
/// Album to display from to show at the site root
static PREVIEW_ALBUM: &str = FAVORITES_ALBUM_NAME;

/// Path-name of the "album" that holds every photo.
///
/// This album is auto-generated, and must not be specified manually in the list of albums.
static ALL_ALBUM_PATH: &str = "all";
static ALL_ALBUM_NAME: &str = "All photos";
static ALL_ALBUM_DESC: &str = "All of my photos on this site, each and every one";
/// Name of the "favorites" album
///
/// We use this to make the displayed content slightly different for photos that are a favorite.
static FAVORITES_ALBUM_NAME: &str = "favorites";

/// Approximate desired pixel count of the smaller versions of images
const SMALL_IMG_APROX_PIXELCOUNT: u64 = 480_000; // ≈ 800x600
/// WEBP quality to encode the small images with
const SMALL_IMG_QUALITY: f32 = 80.0;

/// The value of the 'Cache-Control' header that we set for image requests
///
/// 2592000 seconds is equal to 30 days. It's not infinite, but it's long enough that it doesn't
/// practically matter.
static PHOTO_CACHE_POLICY: &str = "max-age=2592000, immutable";

/// Default map view for the "global" map -- the one containing every photo
const GLOBAL_MAP_VIEW: MapView = MapView {
    centered_at: GPSCoords {
        lat: 37.839,
        lon: -122.396,
    },
    zoom_level: 11,
};

/// Parameters for `FlexGrid` -- refer to 'static/js/flex-grid.js' for more
///
/// A "default" set of values is parsed from 'content/photos/default-flex-grid-config.json', and is
/// what's used in the implementation of [`Default`]. `FlexGridSettings::default` cannot be used
/// before calling [`initialize`].
///
/// All of the fields are renamed during (de-)serialization so that they match the naming of the
/// Javascript constructor.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FlexGridSettings {
    /// Required minimum number of columns
    #[serde(rename = "minColumns")]
    pub min_columns: u64,
    /// Maximum allowed number of columns
    #[serde(rename = "maxColumns")]
    pub max_columns: u64,
    /// Minimum width for a single column, in pixels
    #[serde(rename = "minColumnWidth")]
    pub min_column_width: u64,
    /// The range of values we give for the user to change the column width
    #[serde(rename = "columnWidthRange")]
    pub column_width_range: Range<u64>,
    /// Pixels of padding to put between elements
    pub padding: u64,
    /// Maximum allowed cropping of a single dimension (decreasing either height or width) in order
    /// to create multi-column items
    #[serde(rename = "maxColumnCrop")]
    pub max_column_crop: f64,
    /// Maximum allowed cropping *of* multi-column items in order to get them to fit within the
    /// `max_multi_column_height_multiplier` bound.
    #[serde(rename = "maxMultiCrop")]
    pub max_multi_crop: f64,
    /// Maximum allowed height for items spanning multiple columns, as a multiple of the width of a
    /// single column
    ///
    /// Setting this to zero will disable multi-column items.
    #[serde(rename = "maxMultiColumnHeightMultiplier")]
    pub max_multi_column_height_multiplier: f64,

    /// Maximum allowed number of multi-column images that can be placed over the same columns in a
    /// row
    ///
    /// Must be > 0
    #[serde(rename = "maxSequentialMulti")]
    pub max_sequential_multi: u64,
}

impl Default for FlexGridSettings {
    fn default() -> Self {
        // Cloning is cheap; this type *would* implement `Copy`, but `Range` doesn't.
        DEFAULT_FLEXGRID_SETTINGS.clone()
    }
}

impl FlexGridSettings {
    fn load_default() -> Result<Self> {
        let path = Path::new(IMGS_DIRECTORY).join(FLEXGRID_SETTINGS_FILENAME);
        let file_content = fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to read default `FlexGrid` config from file {:?}",
                path
            )
        })?;

        serde_json::from_str(&file_content)
            .with_context(|| format!("failed to parse `FlexGridSettings` in file {:?}", path))
    }
}

/// Storage type for album information
type AlbumsInformation = Vec<(String, ParsedAlbum)>;

/// Parsed information about an individual album
///
/// The version that we actually store replaces strings for each photo with the reference to the
/// `PhotoInfo` itself. See [`Album`].
#[derive(Deserialize)]
struct ParsedAlbum {
    /// The displayed name of the album
    name: String,
    /// The type of album
    kind: Option<ParsedAlbumKind>,
    /// Whether to display in order from first to last or last to first, from the photos list
    ///
    /// This is only really a feature so that the albums file can sensibly be manually edited.
    /// Eventually, this won't be the case (it'll be handled by dedicated API endpoints).
    display: AlbumDisplayOrder,
    /// A markdown description of the album
    description: String,
    /// The path name of the image to represent this album -- ideally unique, but not required to
    /// be.
    cover_img: String,
    /// Ordered listing of all of the photos. `photos[0]` is displayed first, `photos[1]` second,
    /// etc.
    photos: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
enum ParsedAlbumKind {
    #[serde(rename = "location")]
    Location,
    /// An album for all the photos on a particular day
    #[serde(rename = "day")]
    Day(String),
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize)]
enum AlbumDisplayOrder {
    #[serde(rename = "from_first")]
    FromFirst,
    #[serde(rename = "from_last")]
    FromLast,
}

lazy_static! {
    static ref STATE: RwLock<PhotosState> = RwLock::new(match PhotosState::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to create `PhotosState`: {:#}", e);
            exit(1)
        }
    });
    static ref DEFAULT_FLEXGRID_SETTINGS: FlexGridSettings = match FlexGridSettings::load_default()
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to load default `FlexGridSettings`: {:#}", e);
            exit(1)
        }
    };
}

/// Collects all of the necessary information about the photos we have stored, causing any failures
/// to happen immediately
///
/// Any failures encountered will result in an immediate exit.
pub fn initialize() {
    lazy_static::initialize(&DEFAULT_FLEXGRID_SETTINGS);
    lazy_static::initialize(&STATE);
}

#[get("/")]
pub fn index() -> Template {
    let ctx = STATE.read().unwrap().index_context();
    Template::render(INDEX_TEMPLATE_NAME, ctx)
}

#[get("/albums")]
pub fn albums() -> Template {
    let ctx = STATE.read().unwrap().albums_context();
    Template::render(ALBUMS_TEMPLATE_NAME, ctx)
}

#[get("/view/<name>?<album>")]
pub fn img_page(
    name: Cow<str>,
    album: Option<String>,
) -> Result<MaybeRedirect<Template>, http::Status> {
    let ctx = match STATE.read().unwrap().img_page_context(&name, album)? {
        MaybeRedirect::Dont(c) => c,
        MaybeRedirect::Redirect {
            new_url,
            is_permanent,
        } => {
            return Ok(MaybeRedirect::Redirect {
                new_url,
                is_permanent,
            })
        }
    };

    Ok(MaybeRedirect::Dont(Template::render(
        IMG_TEMPLATE_NAME,
        ctx,
    )))
}

#[get("/album/<name>")]
pub fn album_page(name: Cow<str>) -> Option<Template> {
    let ctx = STATE.read().unwrap().album_context(&name)?;
    Some(Template::render(ALBUM_TEMPLATE_NAME, ctx))
}

#[get("/map")]
pub fn map() -> Template {
    let ctx = STATE.read().unwrap().map_context();
    Template::render(MAP_TEMPLATE_NAME, ctx)
}

pub fn recent_photos_context() -> Vec<Arc<PhotoInfo>> {
    STATE
        .read()
        .unwrap()
        .albums
        .get(PREVIEW_ALBUM)
        .map(|a| a.photos.iter().cloned().take(NUM_PREVIEW_PHOTOS).collect())
        .unwrap_or_default()
}

// We include hashes in the image URLs so that they can be cached forever -- any updates to the
// image will change the hash, so it'll be a different URL.
//
// Per MDN:
//
//     A modern best practice for static resources is to include version/hashes in their URLs,
//     while never modifying the resources — but instead, when necessary, updating the resources
//     with newer versions that have new version-numbers/hashes, so that their URLs are different.
//     That’s called the cache-busting pattern.
//
// https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Cache-Control#immutable
#[get("/img-file/<name>?<size>&<rev>")]
pub fn img(
    name: Cow<str>,
    size: Option<String>,
    rev: Option<String>,
) -> Result<MaybeRedirect<ImageSource>, http::Status> {
    let size = size.unwrap_or_default();

    // The 'size' must be one of `small` or `full`
    let is_full = match size.as_str() {
        "full" => true,
        "small" => false,
        _ => return Err(http::Status::BadRequest),
    };

    let state = STATE.read().unwrap();

    let img = state
        .images
        .get(name.as_ref())
        .ok_or(http::Status::NotFound)?;

    let target_hash = match is_full {
        true => &img.full_img_hash,
        false => &img.smaller_webp.hash,
    };

    let rev_is_some = rev.is_some();
    if *target_hash != rev.unwrap_or_default() {
        return Ok(MaybeRedirect::Redirect {
            new_url: uri!("/photos", img: name, size, target_hash),
            // Only permanently redirect previous revisions. Perma-links to the image might
            // eventually change
            is_permanent: rev_is_some,
        });
    }

    if !is_full {
        Ok(MaybeRedirect::Dont(ImageSource::InMem(
            img.smaller_webp.clone(),
        )))
    } else {
        NamedFile::open(full_img_path(name.as_ref()))
            // We already had an entry for this file; if we couldn't find it, then that's an error on
            // our part.
            .map_err(|_| http::Status::InternalServerError)
            .map(StoredImage)
            .map(ImageSource::File)
            .map(MaybeRedirect::Dont)
    }
}

/// Returns the path of the full image with the given name
fn full_img_path(img_name: &str) -> PathBuf {
    let mut p = Path::new(IMGS_DIRECTORY).join(img_name);
    p.set_extension(FULL_IMG_EXT);
    p
}

impl PhotosState {
    /// Creates the `PhotosState`
    fn new() -> Result<Self> {
        // Step 1
        //
        // Parse the information about the albums & collect album membership for each image
        let (all_albums, all_album_paths) = {
            let parsed = Self::get_albums_info().context("failed to read albums info file")?;

            let names = parsed
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            let all = parsed.into_iter().collect::<HashMap<_, _>>();

            (all, names)
        };

        if all_albums.contains_key(ALL_ALBUM_NAME) {
            bail!(
                "albums info file contains reserved album name {:?}",
                ALL_ALBUM_NAME
            )
        }

        // Photo file name -> unsorted list of album memberships
        let mut album_membership = <HashMap<String, Vec<AlbumReference>>>::new();

        for (path, info) in all_albums.iter() {
            if !is_uri_idempotent(path) {
                bail!(
                    "bad album name {:?}: must URI encode to the same value",
                    path
                );
            }

            for p in &info.photos {
                let album_ref = AlbumReference {
                    path: path.clone(),
                    name: info.name.clone(),
                };

                album_membership
                    .entry(p.clone())
                    .or_default()
                    .push(album_ref);
            }

            // Ensure that all of the album cover images are accounted for by putting them in
            // `album_membership`:
            album_membership.entry(info.cover_img.clone()).or_default();
        }

        let glob_pat = format!("{}/{}", IMGS_DIRECTORY, IMGS_GLOB);
        let candidates = glob(&glob_pat)
            .expect("failed to read glob pattern")
            .map(|glob_result| {
                let path = glob_result.context("failed to get glob item for blog posts")?;

                let file_name: PathBuf = path
                    .file_prefix()
                    .expect("expected glob result to have file name")
                    .into();

                let file_string: String = file_name.to_string_lossy().into();

                if !is_uri_idempotent(&file_string) {
                    bail!(
                        "bad image file name {:?}: must URI encode to the same value",
                        path.file_name().unwrap()
                    );
                }

                // Fetch the albums for this image, if it has any. Doing this now means that we
                // can check -- *before* doing the expensive stuff -- that every image is accounted
                // for.

                let albums = album_membership.remove(&file_string).unwrap_or_default();
                Ok((path, file_string, albums))
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Each image should have claimed its albums. So if there's anything left in
        // `album_membership`, there's images referenced that aren't on disk.
        if !album_membership.is_empty() {
            let referenced_set: Vec<_> = album_membership.keys().collect();
            bail!(
                "some image(s) referenced in albums but aren't on disk: {:?}",
                referenced_set
            );
        }

        let auto_date_albums = Mutex::new(HashMap::new());

        let total_imgs = candidates.len();

        let (tx, rx) = mpsc::channel::<()>();
        let status = thread::spawn(move || {
            let mut seen = 0;
            let mut stdout = io::stdout();

            loop {
                print!("\rProcessing images... {}/{} done", seen, total_imgs);
                let _ = stdout.flush();

                if let Err(_) = rx.recv() {
                    println!("");
                    break;
                }

                seen += 1;
            }
        });

        let images_list_result = candidates
            .into_par_iter()
            .map_with(tx, |tx, (path, file_string, albums)| {
                let info_result = Self::process_photo(
                    &path,
                    &file_string,
                    albums,
                    &all_albums,
                    &auto_date_albums,
                )
                .with_context(|| format!("failed to process photo {:?}", file_string));

                // Send a signal to indicate that we've finished processing this image
                let _ = tx.send(());

                Ok((file_string, Arc::new(info_result?)))
            })
            .collect::<Result<Vec<_>>>();

        // End the status thread
        let _ = status.join(); // shoudn't produce an error, it won't panic

        // And produce the mapping of image names to their infos
        let images: HashMap<_, _> = images_list_result?.into_iter().collect();

        // Earlier, we checked that everything present in `albums` *was* a key in
        // `album_membership`; we can now go through the albums & all of their referenced image
        // names will be present in `images`.

        let mut albums = all_albums
            .into_iter()
            .map(|(path, parsed)| {
                let mut a = Album {
                    name: parsed.name,
                    path: path.clone(),
                    cover_img: images[&parsed.cover_img].clone(),
                    description: markdown_to_html(&parsed.description),
                    photos: parsed
                        .photos
                        .into_iter()
                        .map(|p| images[&p].clone())
                        .collect(),
                    kind: parsed.kind.map(|k| k.into()),
                };

                if parsed.display == AlbumDisplayOrder::FromLast {
                    a.photos.reverse();
                }

                (path, Arc::new(a))
            })
            .chain(
                auto_date_albums
                    .into_inner()
                    .unwrap()
                    .into_iter()
                    .map(|(_, auto)| {
                        let photos: Vec<_> =
                            auto.photos.values().map(|p| images[p].clone()).collect();
                        let a = Arc::new(Album {
                            path: auto.path.clone(),
                            name: auto.name,
                            description: markdown_to_html(&auto.description),
                            cover_img: photos[0].clone(),
                            photos,
                            kind: Some(AlbumKind::Day),
                        });
                        (auto.path, a)
                    }),
            )
            .collect::<HashMap<String, Arc<Album>>>();

        // Finally, add in the album for all of the images
        let images_sorted = {
            let mut imgs: Vec<_> = images.values().cloned().collect();
            // Sort so that later images come first
            imgs.sort_by(|x, y| {
                x.exif_info
                    .actual_datetime
                    .cmp(&y.exif_info.actual_datetime)
                    .reverse()
            });
            imgs
        };

        let midpoint_img = images_sorted[images_sorted.len() / 2].clone();
        albums.insert(
            ALL_ALBUM_PATH.into(),
            Arc::new(Album {
                path: ALL_ALBUM_PATH.into(),
                name: ALL_ALBUM_NAME.to_owned(),
                cover_img: midpoint_img,
                description: ALL_ALBUM_DESC.to_owned(),
                kind: Some(AlbumKind::All),
                photos: images_sorted,
            }),
        );

        let mut images_by_time = images.values().cloned().collect::<Vec<_>>();
        images_by_time.sort_by_key(|img| img.exif_info.actual_datetime);

        let mut albums_in_order = AlbumsInOrder::default();

        for a_path in all_album_paths {
            let a = albums[&a_path].clone();

            let list = match a.kind {
                None | Some(AlbumKind::All) => &mut albums_in_order.normal_albums,
                Some(AlbumKind::Day) => &mut albums_in_order.days,
                Some(AlbumKind::Location) => &mut albums_in_order.locations,
            };

            list.push(a);
        }

        Ok(PhotosState {
            albums,
            albums_in_order,
            images,
            images_by_time,
        })
    }

    /// Reads and parses the album info file
    fn get_albums_info() -> Result<AlbumsInformation> {
        let path = Path::new(IMGS_DIRECTORY).join(Path::new(ALBUMS_META_FILENAME));
        let content = fs::read_to_string(path)?;

        Ok(serde_json::from_str(&content)?)
    }

    fn process_photo(
        file_path: &Path,
        file_string: &str,
        mut albums: Vec<AlbumReference>,
        all_albums: &HashMap<String, ParsedAlbum>,
        auto_date_albums: &Mutex<HashMap<Date<FixedOffset>, AutoDateAlbumBuilder>>,
    ) -> Result<PhotoInfo> {
        let img_data =
            fs::read(&file_path).with_context(|| format!("failed to read file {:?}", file_path))?;

        let exif_info = PhotoExifInfo::from_img_data(&img_data)
            .with_context(|| format!("failed to get photo metadata for file {:?}", file_path))?;

        // Extract the location album from the list, if there is a single one. If there's more
        // than one, return error:
        let location_album_idx = albums
            .iter()
            .enumerate()
            .filter(|(_, r)| all_albums[&r.path].kind == Some(ParsedAlbumKind::Location))
            .map(|(i, _)| i)
            .try_fold(None, Self::fold_extract_single)
            .map_err(|()| anyhow!("found multiple 'location' albums containing this image"))
            .with_context(|| format!("failed to process photo {:?}", file_string))?;
        let location = location_album_idx.map(|i| albums.remove(i));

        let maybe_day_album = albums
            .iter()
            .filter(|r| matches!(all_albums[&r.path].kind, Some(ParsedAlbumKind::Day(_))))
            .try_fold(None, Self::fold_extract_single)
            .map_err(|()| anyhow!("found multiple 'day' albums containing this image"))
            .with_context(|| format!("failed to process photo {:?}", file_string))?;

        let day_album = match maybe_day_album {
            Some(r) => r.clone(),
            // If there wasn't already a "day album" assigned to this photo, we need to use the
            // actual date & get a created-by-default album
            None => {
                let date = exif_info.actual_datetime.date();

                let mut guard = auto_date_albums.lock().unwrap();
                match guard.entry(date) {
                    Entry::Vacant(v) => {
                        let mut album = AutoDateAlbumBuilder::new(date);

                        if all_albums.contains_key(&album.path) {
                            bail!("preexisting album path {:?} conflicts with auto-generated date path", &album.path)
                        }

                        album
                            .photos
                            .insert(exif_info.actual_datetime, file_string.to_owned());
                        v.insert(album).reference()
                    }
                    Entry::Occupied(mut o) => {
                        let album = o.get_mut();
                        album
                            .photos
                            .insert(exif_info.actual_datetime, file_string.to_owned());
                        album.reference()
                    }
                }
            }
        };

        // We sort the remaining album names, just so they happen to display a little nicer
        // (and consistently); the order from the hashmap isn't guaranteed anyways.
        albums.sort_by(|rx, ry| rx.name.cmp(&ry.name));

        let mut is_favorite = false;

        let favorite_idx = albums.binary_search_by_key(&FAVORITES_ALBUM_NAME, |a| a.path.as_str());
        if let Ok(i) = favorite_idx {
            is_favorite = true;
            albums.remove(i);
        }

        let hash = Self::hash(&img_data);

        let smaller_webp = Self::make_smaller_img(&img_data)
            .with_context(|| format!("could not create small image for file {:?}", file_path))?;

        Ok(PhotoInfo {
            file_name: file_string.to_owned(),
            exif_info,
            is_favorite,
            albums,
            location,
            day_album,
            smaller_webp,
            full_img_hash: hash,
        })
    }

    /// Helper function for [`Iterator::try_fold`] to extract an item from an iterator only if
    /// there's exactly one
    fn fold_extract_single<T>(acc: Option<T>, val: T) -> Result<Option<T>, ()> {
        match acc {
            None => Ok(Some(val)),
            Some(_) => Err(()),
        }
    }

    /// Returns the base64-encoded sha256 hash of the data
    ///
    /// The hashing function is subject to change, though sha256 seems to be the best version for
    /// us right now.
    ///
    /// The returned string is URL-safe.
    fn hash(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(data);

        base64::encode_config(hasher.finalize(), base64::URL_SAFE_NO_PAD)
    }

    /// Creates a smaller version of the image - or returns the existing one, if it's already
    /// small enough.
    ///
    /// The input image is expected to be JPEG encoded; the output `InMemImg` will be WEBP, and
    /// will not have the maximum quality.
    fn make_smaller_img(bigger_img_data: &[u8]) -> Result<InMemImg> {
        use image::codecs::jpeg::JpegDecoder;
        use image::imageops::FilterType;
        use image::{DynamicImage, GenericImageView};

        let mut img = JpegDecoder::new(bigger_img_data)
            .and_then(DynamicImage::from_decoder)
            .context("failed to construct source JPEG image")?;

        let (cur_width, cur_height) = {
            let (w, h) = img.dimensions();
            (w as u64, h as u64)
        };

        let current_pixelcount = cur_width * cur_height;

        if current_pixelcount > SMALL_IMG_APROX_PIXELCOUNT {
            let scale = (SMALL_IMG_APROX_PIXELCOUNT as f32 / current_pixelcount as f32).sqrt();

            let new_width = (cur_width as f32 * scale) as u32;
            let new_height = (cur_height as f32 * scale) as u32;

            // img.resize will actually ensure that the aspect ratio is upheld, so we don't
            // *really* need to compute both the width and height. But doing that anyways is easier
            // to explain.
            img = img.resize(new_width, new_height, FilterType::CatmullRom);
        }

        let webp_repr = webp::Encoder::from_image(&img)
            .map_err(|e| anyhow!("{}", e))
            .context("failed to encode WEBP image")?
            .encode(SMALL_IMG_QUALITY);

        let (width, height) = img.dimensions();
        let img_data = Arc::from(webp_repr.to_vec().into_boxed_slice());
        let hash = Self::hash(&img_data);

        Ok(InMemImg {
            height,
            width,
            hash,
            img_data,
        })
    }
}

/// Helper type for constructing the albums that are auto-generated for dates that don't otherwise
/// have one
struct AutoDateAlbumBuilder {
    path: String,
    name: String,
    description: String,
    photos: BTreeMap<DateTime<FixedOffset>, String>,
}

impl AutoDateAlbumBuilder {
    fn new(date: Date<FixedOffset>) -> Self {
        // e.g. 18 December, 2021
        let name = date.format("%-d %B, %Y").to_string();
        let description = format!("<p>Everything from {}</p>", name);

        AutoDateAlbumBuilder {
            // YYYY-MM-DD, e.g. 2021-12-18
            path: date.format("%Y-%m-%d").to_string(),
            name,
            description,
            photos: BTreeMap::new(),
        }
    }

    fn reference(&self) -> AlbumReference {
        AlbumReference {
            path: self.path.clone(),
            name: self.name.clone(),
        }
    }
}

impl PhotoExifInfo {
    /// Parses the exif data in the file into the photo's information.
    ///
    /// Returns an error on EXIF errors or when the data doesn't meet our expectations.
    fn from_img_data(contents: &[u8]) -> Result<Self> {
        let exif = exif::Reader::new()
            // We need to pass the entire contents here as an *owned* vector because EXIF data can
            // be arbitrarily placed within an image; it's not a simple header.
            .read_from_container(&mut Cursor::new(contents))
            .context("failed to read exif data")?;

        let datetime =
            Self::get_local_datetime(&exif).context("failed to construct local DateTime")?;

        let (description, alt_text) = Self::get_description(&exif)
            .context("failed to get photo description")?
            .map(|desc| {
                if !desc.starts_with(ALT_TEXT_PREFIX) {
                    return (Some(markdown_to_html(&desc)), None);
                }

                // Otherwise, extract the alt text from the beginning of the first line
                //
                // I'm not too concerned about using '\n' here instead of something more robust
                // that accounts for DOS's "\r\n", just because I'm doing the work here on Unix.
                // If, for some reason, you're using this on Windows... just don't. Save yourself
                // the trouble.
                //
                // (or submit a PR - this is one of the few things I won't fix for you).
                let (first_line, rest) = match desc.split_once('\n') {
                    Some((f, t)) => (f, t),
                    // If the only line in the description starts with the alt text prefix, then we
                    // have alt text, but no description.
                    None => return (None, Some(desc)),
                };

                (
                    Some(first_line[ALT_TEXT_PREFIX.len()..].to_owned()),
                    Some(markdown_to_html(rest)),
                )
            })
            .unwrap_or((None, None));

        Ok(PhotoExifInfo {
            title: Self::get_title(&exif).context("failed to get photo title")?,
            description,
            alt_text,
            coords: Self::get_gps_coords(&exif).context("failed to get GPS coordinates")?,
            camera: CameraInfo {
                id: Self::get_camera_id(&exif).context("failed to get camera name")?,
                lens_id: Self::get_lens_id(&exif).context("failed to get lens ID")?,
                iso: Self::get_iso(&exif).context("failed to get camera ISO")?,
                f_stop: Self::get_f_stop(&exif).context("failed to get camera F-Stop")?,
                focal_length: Self::get_focal_length(&exif)
                    .context("failed to get camera focal length")?,
                exposure_time: Self::get_exposure_time(&exif)
                    .context("failed to get camera exposure time")?,
            },
            actual_datetime: datetime,
            local_time: format_datetime(datetime, FormatLevel::LocalTime),
            tz_offset: format_datetime(datetime, FormatLevel::Offset),
            date: format_datetime(datetime, FormatLevel::Date),
        })
    }

    fn get_title(exif: &exif::Exif) -> Result<String> {
        use exif::{In, Tag, Value};

        // The EXIF (2.2) specification describes the `ImageDescription` field with:
        //
        //     A character string giving the title of the image. It may be a comment such as "1988
        //     company picnic" or the like. Two-byte character codes cannot be used. When a 2-byte
        //     code is necessary, the Exif Private tag `UserComment` is to be used
        //
        // Its value is expected to be ASCII data, but it does not specify how many ASCII slices we
        // can get. For our purposes, we just require one, even though it's not strictly necessary;
        // it makes the semantics easier.
        //
        // https://www.exif.org/Exif2-2.PDF

        let title_value = &exif
            .get_field(Tag::ImageDescription, In::PRIMARY)
            .ok_or_else(|| anyhow!("missing Description tag"))?
            .value;

        let title = match title_value {
            Value::Ascii(vs) if vs.len() == 1 => &vs[0],
            Value::Ascii(_) => bail!(
                "expected single-length ASCII value in Description tag, found {:?}",
                title_value
            ),
            _ => bail!(
                "expected ASCII value for Description tag, found {:?}",
                title_value
            ),
        };

        // We require that the description is non-empty, because this is the thing we actually
        // display on the webpage to title the image
        if title.is_empty() {
            bail!("empty Description field");
        }

        // The "ASCII" field is really supposed to just be ASCII
        if !title.is_ascii() {
            bail!("non-ASCII bytes in Description field: {:?}", title);
        }

        // ASCII is a subset of utf-8
        Ok(String::from_utf8(title.clone()).unwrap())
    }

    /// Retrieves the description of the image from the EXIF data & converts it to HTML
    fn get_description(exif: &exif::Exif) -> Result<Option<String>> {
        use exif::{In, Tag, Value};

        // Contrary to what the name might lead you to believe, the `ImageDescription` tag is not
        // actually supposed to give a description of the image. (refer to the comment in
        // `get_title`).
        //
        // So we use `UserComment`. This is what the EXIF (2.2) specification says about it:
        //
        //     A tag for Exif users to write keywords or comments on the image besides those in
        //     ImageDescription, and without the character code limitations of the ImageDescription
        //     tag.
        //
        // The specification dictates that the type of data must be `Undefined`, which has its own
        // rules for what goes in it. The first 8 bytes inform how we interpret the rest, and must
        // be one of:
        //
        //
        //     First 8 bytes              | Rest of the content
        //     ---------------------------|-------------------
        //     b"ASCII\x00\x00\x00"       | ASCII text
        //     b"JIS\x00\x00\x00\x00\x00" | JIS-encoded text
        //     b"UNICODE\x00"             | UTF-16 (LE?)
        //     [0, 0, 0, 0, 0, 0, 0, 0]   | <Undefined>
        //
        // https://www.exif.org/Exif2-2.PDF

        let desc = match exif.get_field(Tag::UserComment, In::PRIMARY) {
            None => return Ok(None),
            Some(f) => match &f.value {
                Value::Undefined(v, _) => v,
                v => bail!(
                    "expected Undefined value for UserComment tag, found {:?}",
                    v
                ),
            },
        };

        if desc.is_empty() {
            return Ok(None);
        }

        let md = match desc.get(..8) {
            Some(b"ASCII\x00\x00\x00") => std::str::from_utf8(&desc[8..])
                .map(Cow::Borrowed)
                .context("UserComment tag was not valid UTF-8")?,
            Some(b"JIS\x00\x00\x00\x00\x00") => {
                bail!("unsupported JIS encoding for UserComment tag")
            }
            Some(b"UNICODE\x00") => {
                // String::from_utf16 requires that we give it u16s, so we have to convert tothem
                // first.
                //
                // On my little-endian system, exiftool outputs little-endian UTF-16, so we'll
                // assume that's what we're looking for. If it's not little-endian, then oh well --
                // we'll just give an error. I can fix it later pretty easily.
                //
                // See:
                //
                //   "It is also reliable to detect endianness by looking for null bytes, on the
                //    assumption that characters less than U+0100 are very common. If more even
                //    bytes (starting at 0) are null, then it is big-endian"
                //
                // https://en.wikipedia.org/wiki/UTF-16#Byte-order_encoding_schemes

                let s = &desc[8..];
                if s.len() % 2 != 0 {
                    bail!("odd length on UserComment tag's UTF-16 content");
                }

                let u16_len = s.len() / 2;
                let mut v = vec![0_u16; u16_len];

                // Little-endian conversion (u8, u8) -> u16
                for i in 0..u16_len {
                    v[i] = s[i * 2] as u16;
                    v[i] |= (s[i * 2 + 1] as u16) << 8;
                }

                String::from_utf16(&v)
                    .map(Cow::Owned)
                    .context("UserComment tag was not valid UTF-16 LE")?
            }
            Some([0, 0, 0, 0, 0, 0, 0, 0]) => {
                bail!("unsupported 'Undefined' encoding for UserComment tag")
            }
            _ => bail!(
                "expected character code for UserComment tag, found {:?}",
                desc
            ),
        };

        if md.is_empty() {
            return Ok(None);
        }

        Ok(Some(markdown_to_html(md.as_ref())))
    }

    fn get_gps_coords(exif: &exif::Exif) -> Result<Option<GPSCoords>> {
        use exif::Tag;

        let lat =
            Self::gps_decimal(exif, Tag::GPSLatitude).context("could not read GPSLatitude tag")?;
        let lon = Self::gps_decimal(exif, Tag::GPSLongitude)
            .context("could not read GPSLongitude tag")?;
        let lat_sign = Self::gps_ref(exif, Tag::GPSLatitudeRef, "N", "S")
            .context("could not read GPSLatitudeRef tag")?;
        let lon_sign = Self::gps_ref(exif, Tag::GPSLongitudeRef, "E", "W")
            .context("could not read GPSLongitudeRef tag")?;

        match (lat, lon, lat_sign, lon_sign) {
            (Some(lat), Some(lon), Some(lat_sign), Some(lon_sign)) => Ok(Some(GPSCoords {
                lat: lat * lat_sign,
                lon: lon * lon_sign,
            })),
            (None, None, None, None) => Ok(None),
            // If only *some* of the tags are missing, that's an error; we should have all or
            // nothing.
            _ => {
                let missing = [
                    (lat.is_some(), "GPSLatitude"),
                    (lat_sign.is_some(), "GPSLatitudeRef"),
                    (lon.is_some(), "GPSLongitude"),
                    (lon_sign.is_some(), "GPSLongitudeRef"),
                ]
                .into_iter()
                .filter(|(is_some, _)| *is_some)
                .map(|(_, name)| name)
                .collect::<Vec<_>>();

                bail!("partial GPS tags: missing {:?}", missing);
            }
        }
    }

    /// On success, returns -1 or 1, corresponding to the indicated direction of the GPS tag
    ///
    /// Returns `Ok(None)` if the tag isn't present, or `Err(_)` if the tag is malformed (the
    /// to be ASCII)
    fn gps_ref(exif: &exif::Exif, tag: exif::Tag, pos: &str, neg: &str) -> Result<Option<f64>> {
        use exif::{In, Value};

        // The EXIF specification says that the GPS*Ref tags should be ASCII values with a count of
        // 2. Exiftool only emits one, so that's what we'll expect.

        let value = match exif.get_field(tag, In::PRIMARY) {
            Some(f) => &f.value,
            None => return Ok(None),
        };

        match value {
            Value::Ascii(vs) if vs.len() == 1 => match &vs[0] {
                v if v == pos.as_bytes() => Ok(Some(1.0)),
                v if v == neg.as_bytes() => Ok(Some(-1.0)),
                v => bail!("expected either {:?} or {:?}, found {:?}", pos, neg, v),
            },
            Value::Ascii(_) => bail!("expected a single ASCII value"),
            _ => bail!("expected an ascii value"),
        }
    }

    /// On success, returns a positive value corresponding to the magnitude of the GPS coordinate
    ///
    /// The direction can be fetched with [`gps_ref`]. Returns `Ok(None)` if the tag isn't present,
    /// or `Err(_)` if the tag is malformed.
    ///
    /// The EXIF specification requires that this tag has a value of 3 rationals (degrees minutes
    /// seconds). This function converts that to decimal degrees.
    ///
    /// [`gps_ref`]: Self::gps_ref
    fn gps_decimal(exif: &exif::Exif, tag: exif::Tag) -> Result<Option<f64>> {
        use exif::{In, Value};

        let value = match exif.get_field(tag, In::PRIMARY) {
            Some(f) => &f.value,
            None => return Ok(None),
        };

        let (deg, min, sec) = match value {
            Value::Rational(vs) if vs.len() == 3 => (vs[0], vs[1], vs[2]),
            Value::Rational(_) => bail!("unexpected number of Rationals, found {:?}", value),
            _ => bail!("expected 3 Rationals, found {:?}", value),
        };

        // Conversion from DMS -> DD is pretty ok. Essentially, "degrees" are hours, and the others
        // have the expected relative sizes.
        //
        // So: DD = D + M / 60 + S / 3600

        let dd = (deg.num as f64 / deg.denom as f64)
            + (min.num as f64 / 60.0 / min.denom as f64)
            + (sec.num as f64 / 3600.0 / sec.denom as f64);

        Ok(Some(dd))
    }

    fn get_local_datetime(exif: &exif::Exif) -> Result<DateTime<FixedOffset>> {
        use exif::{In, Tag, Value};

        // We use DateTimeOriginal/OffsetTimeOriginal here because that corresponds to the actual
        // time that the photo was taken
        //
        // See: https://mail.gnome.org/archives/f-spot-list/2005-August/msg00081.html
        let datetime_value = &exif
            .get_field(Tag::DateTimeOriginal, In::PRIMARY)
            .ok_or_else(|| anyhow!("missing DateTimeOriginal field"))?
            .value;

        let raw_datetime;

        let mut dt = match datetime_value {
            Value::Ascii(ds) if ds.len() == 1 => {
                raw_datetime = &ds[0];

                exif::DateTime::from_ascii(&ds[0])
                    .context("failed to parse DateTimeOriginal tag")?
            }
            Value::Ascii(_) => bail!(
                "expected single ASCII value in DateTimeOriginal tag, found {:?}",
                datetime_value
            ),
            _ => bail!(
                "expected ASCII value for DateTimeOriginal tag, found {:?}",
                datetime_value
            ),
        };

        let offset_value = &exif
            .get_field(Tag::OffsetTimeOriginal, In::PRIMARY)
            .ok_or_else(|| anyhow!("missing OffsetTimeOriginal field"))?
            .value;

        let raw_offset;

        match offset_value {
            Value::Ascii(vs) if vs.len() == 1 => {
                raw_offset = &vs[0];

                dt.parse_offset(&vs[0])
                    .context("failed to parse OffsetTimeOriginal tag")?
            }
            Value::Ascii(_) => bail!(
                "expected single ASCII value in OffsetTimeOriginal tag, found {:?}",
                offset_value
            ),
            _ => bail!(
                "expected ASCII value for OffsetTimeOriginal tag, found {:?}",
                offset_value
            ),
        }

        let offset_seconds = dt.offset.unwrap() as i32 * 60;

        let offset = FixedOffset::east_opt(offset_seconds).ok_or_else(|| {
            anyhow!(
                "invalid offset {:?}",
                std::str::from_utf8(raw_offset).unwrap()
            )
        })?;

        let final_datetime = offset
            .ymd_opt(dt.year as i32, dt.month as u32, dt.day as u32)
            .and_hms_nano_opt(
                dt.hour as u32,
                dt.minute as u32,
                dt.second as u32,
                dt.nanosecond.unwrap_or(0),
            )
            .single()
            .ok_or_else(|| {
                anyhow!(
                    "invalid date {:?}",
                    std::str::from_utf8(raw_datetime).unwrap()
                )
            })?;

        Ok(final_datetime)
    }

    /// Helper function to extract a non-empty ascii string from an EXIF value
    fn extract_nonempty_ascii_from_exif(value: &exif::Value, tag_name: &str) -> Result<String> {
        use exif::Value;

        let v = match value {
            // We can't directly match with a slice pattern, because it's a vector :(
            Value::Ascii(vs) if vs.len() == 1 => &vs[0],
            _ => bail!(
                "expected single-length ASCII value in {} tag, found {:?}",
                tag_name,
                value,
            ),
        };

        if v.is_empty() {
            bail!("empty {} field", tag_name);
        } else if !v.is_ascii() {
            bail!("non-ASCII bytes in {} field {:?}", tag_name, v);
        }

        // ASCII is a subset of utf-8; this should always hold.
        Ok(String::from_utf8(v.clone()).unwrap())
    }

    fn get_camera_id(exif: &exif::Exif) -> Result<(String, String)> {
        use exif::{In, Tag};

        let make = exif
            .get_field(Tag::Make, In::PRIMARY)
            .map(|v| Self::extract_nonempty_ascii_from_exif(&v.value, "Make"))
            .transpose()?
            .ok_or_else(|| anyhow!("missing (camera) Make tag"))?;

        let mut model = exif
            .get_field(Tag::Model, In::PRIMARY)
            .map(|v| Self::extract_nonempty_ascii_from_exif(&v.value, "Model"))
            .transpose()?
            .ok_or_else(|| anyhow!("missing (camera) Model tag"))?;

        if let Some(stripped) = model.strip_prefix(make.as_str()) {
            model = stripped.trim_start().to_owned();
        }

        Ok((make, model))
    }

    fn get_lens_id(exif: &exif::Exif) -> Result<Option<(String, String)>> {
        use exif::{In, Tag};

        let make = exif
            .get_field(Tag::LensMake, In::PRIMARY)
            .map(|v| Self::extract_nonempty_ascii_from_exif(&v.value, "LensMake"))
            .transpose()?;

        let model = exif
            .get_field(Tag::LensModel, In::PRIMARY)
            .map(|v| Self::extract_nonempty_ascii_from_exif(&v.value, "LensModel"))
            .transpose()?;

        match (make, model) {
            (None, None) => return Ok(None),
            (Some(make), Some(model)) => return Ok(Some((make, model))),
            (Some(_), None) => bail!("found LensMake tag but no LensModel"),
            (None, Some(_)) => bail!("found LensModel tag but no LensMake"),
        }
    }

    fn get_iso(exif: &exif::Exif) -> Result<u16> {
        use exif::{In, Tag, Value};

        let value = &exif
            // Why 'PhotographicSensitivity'? There's an explanation in the doc comment for
            // `CameraInfo.iso`.
            .get_field(Tag::PhotographicSensitivity, In::PRIMARY)
            .ok_or_else(|| anyhow!("missing PhotographicSensitivity (ISO) tag"))?
            .value;

        // The ISO value is expected to be a short. Maybe this gets messed up for really high ISO,
        // but I'm not sure.
        match value {
            Value::Short(vs) if vs.len() == 1 => Ok(vs[0]),
            // Technically speaking, the EXIF spec allows any number of values here; I'm not sure
            // what more of them means.
            _ => bail!(
                "expected single short value in PhotographicSensitivity tag, found {:?}",
                value
            ),
        }
    }

    fn get_f_stop(exif: &exif::Exif) -> Result<f64> {
        use exif::{In, Tag, Value};

        let value = &exif
            .get_field(Tag::FNumber, In::PRIMARY)
            .ok_or_else(|| anyhow!("missing FNumber (f-stop) tag"))?
            .value;

        match value {
            Value::Rational(vs) if vs.len() == 1 => Ok(vs[0].to_f64()),
            _ => bail!(
                "expected single rational value in FNumber tag, found {:?}",
                value
            ),
        }
    }

    fn get_focal_length(exif: &exif::Exif) -> Result<f64> {
        use exif::{In, Tag, Value};

        let value = &exif
            .get_field(Tag::FocalLength, In::PRIMARY)
            .ok_or_else(|| anyhow!("missing FocalLength tag"))?
            .value;

        match value {
            Value::Rational(vs) if vs.len() == 1 => Ok(vs[0].to_f64()),
            _ => bail!(
                "expected single rational value in FocalLength tag, found {:?}",
                value
            ),
        }
    }

    fn get_exposure_time(exif: &exif::Exif) -> Result<String> {
        use exif::{In, Tag, Value};

        let value = &exif
            .get_field(Tag::ExposureTime, In::PRIMARY)
            .ok_or_else(|| anyhow!("missing ExposureTime tag"))?
            .value;

        let rat = match value {
            Value::Rational(vs) if vs.len() == 1 => vs[0],
            _ => bail!(
                "expected single rational value in FocalLength tag, found {:?}",
                value
            ),
        };

        // If the numerator is 1, then we can do a fractional formatting, e.g. 1/10
        if rat.num == 1 {
            return Ok(format!("1/{}", rat.denom));
        }

        // Otherwise, we should probably just represent the duration as a fraction directly:
        Ok(rat.to_f64().to_string())
    }
}

struct PhotosState {
    // There are a couple of special albums -- namely "all" and "favorites". These are only handled
    // as special cases during construction; they're accessed normally.
    albums: HashMap<String, Arc<Album>>,
    // Every *manually created* album, separated by type and in the order that they were given in
    // the original file
    albums_in_order: AlbumsInOrder,
    // "path name" -> image
    images: HashMap<String, Arc<PhotoInfo>>,
    // All images, sorted by the time they were taken
    images_by_time: Vec<Arc<PhotoInfo>>,
}

#[derive(Clone, Default, Serialize)]
struct AlbumsInOrder {
    normal_albums: Vec<Arc<Album>>,
    days: Vec<Arc<Album>>,
    locations: Vec<Arc<Album>>,
}

#[derive(Serialize)]
struct IndexContext {
    favorites: Arc<Album>,
    flex_grid_settings: FlexGridSettings,
}

#[derive(Serialize)]
struct ImagePageContext {
    album: Option<String>,
    img: Arc<PhotoInfo>,
    previous: Option<Arc<PhotoInfo>>,
    next: Option<Arc<PhotoInfo>>,
    map_view: Option<MapView>,
}

/// The initial view of a photos map on a page
#[derive(Serialize)]
struct MapView {
    #[serde(rename = "centeredAt")]
    centered_at: GPSCoords,
    #[serde(rename = "zoomLevel")]
    zoom_level: u8,
}

#[derive(Serialize)]
struct AlbumContext {
    #[serde(flatten)]
    album: Arc<Album>,
    flex_grid_settings: FlexGridSettings,
}

#[derive(Serialize)]
struct MapContext {
    photos: Vec<Arc<PhotoInfo>>,
    map_view: MapView,
}

impl PhotosState {
    fn index_context(&self) -> IndexContext {
        IndexContext {
            favorites: self.albums[FAVORITES_ALBUM_NAME].clone(),
            flex_grid_settings: DEFAULT_FLEXGRID_SETTINGS.clone(),
        }
    }

    fn albums_context(&self) -> AlbumsInOrder {
        self.albums_in_order.clone()
    }

    fn img_page_context(
        &self,
        img: &str,
        album: Option<String>,
    ) -> Result<MaybeRedirect<ImagePageContext>, http::Status> {
        let img_info = self
            .images
            .get(img)
            .cloned()
            .ok_or(http::Status::NotFound)?;

        let album_ref = album.as_ref().map(|s| s.as_str());

        let img_list = match album_ref {
            None => &self.images_by_time,
            Some(name) => match self.albums.get(name) {
                None => {
                    return Ok(MaybeRedirect::Redirect {
                        new_url: uri!("/photos", img_page: Cow::Borrowed(img), ""),
                        is_permanent: false,
                    })
                }
                Some(a) => &a.photos,
            },
        };

        // Find the point in the image list at which this image is located
        let (idx, _) = img_list
            .iter()
            .enumerate()
            .find(|(_, im)| Arc::ptr_eq(im, &img_info))
            .unwrap_or_else(|| {
                panic!(
                    "failed to find image '{}' in album {}",
                    img_info.file_name,
                    album_ref.unwrap_or("all"),
                )
            });

        let previous = idx.checked_sub(1).map(|i| img_list[i].clone());
        let next = img_list.get(idx + 1).cloned();

        let map_view = img_info.exif_info.coords.map(|c| {
            MapView {
                centered_at: c,
                // Just picking some value for now; we might make this per-image later - who knows.
                zoom_level: 12,
            }
        });

        Ok(MaybeRedirect::Dont(ImagePageContext {
            album,
            img: img_info,
            next,
            previous,
            map_view,
        }))
    }

    fn album_context(&self, name: &str) -> Option<AlbumContext> {
        Some(AlbumContext {
            album: self.albums.get(name)?.clone(),
            flex_grid_settings: FlexGridSettings::default(),
        })
    }

    fn map_context(&self) -> MapContext {
        MapContext {
            photos: self.images_by_time.clone(),
            map_view: GLOBAL_MAP_VIEW,
        }
    }
}

/// Stored information about an individual album
#[derive(Debug, Clone, Serialize)]
struct Album {
    /// The displayed name of the album
    name: String,
    /// The path name of the album
    path: String,
    /// A markdown description of the album
    description: String,
    /// The kind of album, if it's anything notable
    kind: Option<AlbumKind>,
    /// The image used to represent this album -- ideally unique, but not strictly required to be
    cover_img: Arc<PhotoInfo>,
    /// Ordered listing of all of the photos. `photos[0]` is displayed first, `photos[1]` second,
    /// etc.
    photos: Vec<Arc<PhotoInfo>>,
}

#[derive(Debug, Copy, Clone, Serialize)]
enum AlbumKind {
    Day,
    Location,
    All,
}

impl From<ParsedAlbumKind> for AlbumKind {
    fn from(parsed: ParsedAlbumKind) -> AlbumKind {
        match parsed {
            ParsedAlbumKind::Day(_) => AlbumKind::Day,
            ParsedAlbumKind::Location => AlbumKind::Location,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct PhotoExifInfo {
    /// The human-compatible title of the photo, often similar to the file name in `PhotoInfo`
    title: String,
    /// The HTML string corresponding to the original markdown description of the photo
    ///
    /// Will be `None` if not originally provided
    description: Option<String>,

    /// The alt text for the image, if provided -- it'll be parsed from the same EXIF field as the
    /// description.
    alt_text: Option<String>,

    coords: Option<GPSCoords>,

    /// Metadata about the camera that took the photo
    camera: CameraInfo,

    /// The actual date & time at which the photo was taken, preserved so that we can use it for
    /// comparisons & date extraction later
    #[serde(skip)]
    actual_datetime: DateTime<FixedOffset>,

    /// The local time at which the photo was taken, excluding offset
    local_time: String,
    /// The timezone offset at which the photo was taken
    tz_offset: String,
    /// The date on which the photo was taken; can be derived from `actual_datetime`, but stored
    /// here for convenience.
    date: String,
}

/// Information about the camera (and its settings) for a particular photo
#[derive(Debug, Clone, Serialize)]
struct CameraInfo {
    /// Taken from the `Make` and `Model` EXIF tags, the manufacturer and name of the camera
    ///
    /// Most of the time, the make of the camera will prefix the model name; so we intentionally
    /// strip that where we can.
    id: (String, String),

    /// The `LensMake` and `LensModel` EXIF tags; the manufacturer and model of the lens attached
    /// to the camera, if there was one
    ///
    /// This is a little tricky, because some cameras don't actually set both of these fields. It's
    /// an error to have only one, so it often has to be set manually (specifically the `LensMake`
    /// tag).
    ///
    /// The pair is `(LensMake, LensModel)`.
    lens_id: Option<(String, String)>,

    /// Taken from the `PhotographicSensitivity` EXIF tag
    ///
    /// The naming of the tag is a little weird; it was previously called `ISOSpeedRatings` in EXIF
    /// 2.2, then changed to the this name in EXIF 2.3 -- at least according to the ExifTool
    /// source:
    ///
    /// https://github.com/exiftool/exiftool/blob/74dbab1d2766d6422bb05b033ac6634bf8d1f582/lib/Image/ExifTool/Exif.pm#L1943-L1947
    iso: u16,

    /// Taken from the `FNumber` EXIF tag
    f_stop: f64,

    /// The focal length of the camera, *without* translating to 35mm film format
    focal_length: f64,

    /// The exposure time for the photo, in seconds; e.g. `1/30` or `10`.
    exposure_time: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhotoInfo {
    file_name: String,

    #[serde(flatten)]
    exif_info: PhotoExifInfo,

    is_favorite: bool,
    albums: Vec<AlbumReference>,
    location: Option<AlbumReference>,
    day_album: AlbumReference,

    #[serde(rename = "smaller")]
    smaller_webp: InMemImg,

    // The sha256 hash of the full image, base64 encoded
    full_img_hash: String,
}

#[derive(Debug, Clone, Serialize)]
struct AlbumReference {
    /// The "path name" of the album, used in URL references to it
    path: String,
    /// The pretty-printed, displayed name of the album
    name: String,
}

#[derive(Clone, Serialize)]
pub struct InMemImg {
    height: u32,
    width: u32,

    // Like the hash in `PhotoInfo`, but just for this one.
    hash: String,

    /// The WEBP-encoded image
    #[serde(skip)]
    img_data: Arc<[u8]>,
}

impl Debug for InMemImg {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct("InMemImg")
            .field("height", &self.height)
            .field("width", &self.width)
            .field("hash", &self.hash)
            .field("img_data (len)", &self.img_data.len())
            .finish()
    }
}

#[derive(Debug, Copy, Clone, Serialize)]
struct GPSCoords {
    lat: f64,
    lon: f64,
}

impl<'r> Responder<'r> for InMemImg {
    fn respond_to(self, _req: &Request) -> response::Result<'r> {
        use http::{uncased::Uncased, ContentType};
        use rocket::Response;

        let mut builder = Response::build();
        builder
            .header(ContentType::WEBP)
            .header(http::Header {
                name: Uncased::new("Cache-Control"),
                value: Cow::Borrowed(PHOTO_CACHE_POLICY),
            })
            .sized_body(Cursor::new(self.img_data));

        Ok(builder.finalize())
    }
}

/// Wrapper around the `NamedFile` responder to set an appropriate cache policy
pub struct StoredImage(NamedFile);

impl<'r> Responder<'r> for StoredImage {
    fn respond_to(self, req: &Request) -> response::Result<'r> {
        use http::uncased::Uncased;

        let mut resp = self.0.respond_to(req)?;

        resp.set_header(http::Header {
            name: Uncased::new("Cache-Control"),
            value: Cow::Borrowed(PHOTO_CACHE_POLICY),
        });

        Ok(resp)
    }
}

/// Wrapper around the different storage (and responder) types
pub enum ImageSource {
    InMem(InMemImg),
    File(StoredImage),
}

impl<'r> Responder<'r> for ImageSource {
    fn respond_to(self, req: &Request) -> response::Result<'r> {
        match self {
            ImageSource::InMem(img) => img.respond_to(req),
            ImageSource::File(f) => f.respond_to(req),
        }
    }
}
