//! The `photos` resource: list, show, create, edit and delete `Photo` records, as Vue pages.
//!
//! In `routes/web.rs`, `index` and `show` are public and the other actions sit behind the `auth` middleware: only
//! signed-in users create, edit and delete records. Any signed-in user may change any record; where records belong
//! to someone, check that in the actions.
//!
//! `index`, `show` and `edit` send every field of the records to the browser as props, where anyone can read them:
//! when the model holds something private, send a struct with only the public fields instead.

use smeltery::alloy::{self, Page};
use smeltery::db::prelude::*;
use smeltery::http::{Redirect, UploadedFile};
use smeltery::session::Session;
use smeltery::validation::Valid;
use smeltery::{App, Result, Validate};

use crate::app::models::{Photo, photo};

/// The fields of the create form, with their validation rules.
#[derive(Debug, Deserialize, Validate)]
pub struct PhotoForm {
    #[validate(required, max = 255)]
    pub title: String,
    #[validate(required, max = 2048, mimes = "jpg,jpeg,png,gif,webp")]
    pub image: Option<UploadedFile>,
    #[validate(max = 2048, mimes = "jpg,jpeg,png,gif,webp,pdf,txt,csv,docx,xlsx")]
    pub scan: Option<UploadedFile>,
    #[serde(default)]
    pub public: bool,
}

/// The fields of the edit form: a file left empty keeps the stored one.
#[derive(Debug, Deserialize, Validate)]
pub struct PhotoUpdateForm {
    #[validate(required, max = 255)]
    pub title: String,
    #[validate(max = 2048, mimes = "jpg,jpeg,png,gif,webp")]
    pub image: Option<UploadedFile>,
    #[validate(max = 2048, mimes = "jpg,jpeg,png,gif,webp,pdf,txt,csv,docx,xlsx")]
    pub scan: Option<UploadedFile>,
    #[serde(default)]
    pub public: bool,
}

/// Where uploads go, under `storage/app` (served at `/storage/photos/…` after `smeltery storage:link`).
///
/// The `mimes` rules of the forms list the file types accepted. Add types there as needed, but never `html`, `svg`,
/// `xml` or `js`: a browser runs those as pages of this app.
const UPLOADS: &str = "public/photos";

/// Stores an uploaded file in [`UPLOADS`]; returns its path under `storage/app/public`.
async fn store_upload(file: Option<&UploadedFile>) -> Result<Option<String>> {
    let Some(file) = file else {
        return Ok(None);
    };
    let stored = file.store(UPLOADS).await?;
    Ok(Some(
        stored.strip_prefix("public/").unwrap_or(&stored).to_owned(),
    ))
}

/// `GET /photos`: every record (`resources/js/pages/photos/Index.vue`).
pub async fn index(db: Db) -> Result<Page> {
    let photos = Photo::all(&db).await?;
    Ok(alloy::render("photos/Index").with("photos", photos))
}

/// `GET /photos/create`: the create form (`resources/js/pages/photos/Create.vue`).
pub async fn create() -> Page {
    alloy::render("photos/Create")
}

/// `POST /photos`: saves a new record.
pub async fn store(
    app: App,
    db: Db,
    session: Session,
    Valid(form): Valid<PhotoForm>,
) -> Result<Redirect> {
    let image_path = store_upload(form.image.as_ref()).await?;
    let scan_path = store_upload(form.scan.as_ref()).await?;
    Photo::create(
        &db,
        photo::ActiveModel {
            title: Set(form.title),
            image: Set(image_path.unwrap_or_default()),
            scan: Set(scan_path),
            public: Set(form.public),
            ..Default::default()
        },
    )
    .await?;
    session.flash("status", "Photo created.");
    Ok(Redirect::to(&app.url("photos.index", &[])?))
}

/// `GET /photos/{photo}`: one record (`resources/js/pages/photos/Show.vue`).
pub async fn show(Found(photo): Found<Photo>) -> Page {
    alloy::render("photos/Show").with("photo", photo)
}

/// `GET /photos/{photo}/edit`: the edit form (`resources/js/pages/photos/Edit.vue`).
pub async fn edit(Found(photo): Found<Photo>) -> Page {
    alloy::render("photos/Edit").with("photo", photo)
}

/// `PUT /photos/{photo}`: saves the changes.
pub async fn update(
    app: App,
    db: Db,
    session: Session,
    Found(photo): Found<Photo>,
    Valid(form): Valid<PhotoUpdateForm>,
) -> Result<Redirect> {
    let image_path = store_upload(form.image.as_ref()).await?;
    let scan_path = store_upload(form.scan.as_ref()).await?;
    let photo = photo
        .update(&db, |m| {
            m.title = Set(form.title);
            if let Some(path) = image_path {
                m.image = Set(path);
            }
            if let Some(path) = scan_path {
                m.scan = Set(Some(path));
            }
            m.public = Set(form.public);
        })
        .await?;
    session.flash("status", "Photo updated.");
    let id = photo.id.to_string();
    let url = app.url("photos.show", &[("photo", &id)])?;
    Ok(Redirect::to(&url))
}

/// `DELETE /photos/{photo}`: deletes the record.
pub async fn destroy(
    app: App,
    db: Db,
    session: Session,
    Found(photo): Found<Photo>,
) -> Result<Redirect> {
    photo.delete(&db).await?;
    session.flash("status", "Photo deleted.");
    Ok(Redirect::to(&app.url("photos.index", &[])?))
}
