//! The `post_image` Spark: uploads, replaces and removes a post's image; its view is
//! `resources/views/sparks/post_image.mold.html`.

use serde::{Deserialize, Serialize};
use smeltery::db::prelude::{Record as _, Set};
use smeltery::prelude::*;

use crate::app::models::Post;

/// Where images go, under `storage/app` (served at `/storage/posts/…` after `smeltery storage:link`).
const DIR: &str = "public/posts";

/// A post's image. Shown with `@spark("post_image", { post_id: post.id })`.
#[derive(Serialize, Deserialize, Default, Spark, Validate)]
#[spark(name = "post_image")]
pub struct PostImage {
    /// The post (set from the prop; the page cannot change it).
    pub post_id: i64,
    /// The stored image, relative to `storage/app/public`.
    pub image: Option<String>,
    /// The chosen file: at most 2 MB, a PNG, JPEG, GIF or WebP by name; `save` also checks the bytes.
    #[spark(upload(max = 2048, mimes = "png,jpg,jpeg,gif,webp"))]
    #[validate(required)]
    pub photo: Option<TemporaryUpload>,
}

#[actions]
impl PostImage {
    /// First render: load the post's current image.
    pub async fn mount(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        self.image = Post::find_or_404(&ctx.db()?, self.post_id).await?.image;
        Ok(())
    }

    /// `wire:click="save"`: store the chosen file as the post's image, replacing the old one.
    #[guard(auth)]
    pub async fn save(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        ctx.validate(self).await?;
        let Some(photo) = self.photo.take() else {
            return Ok(());
        };
        if !is_image(&photo.bytes(ctx).await?) {
            return Err(Error::validation(
                "photo",
                "The photo must be a PNG, JPEG, GIF or WebP image.",
            ));
        }
        let stored = photo.store(ctx, DIR).await?;
        let image = stored.strip_prefix("public/").unwrap_or(&stored).to_owned();
        let db = ctx.db()?;
        let post = Post::find_or_404(&db, self.post_id).await?;
        let old = post.image.clone();
        let saved = image.clone();
        post.update(&db, |m| m.image = Set(Some(saved))).await?;
        if let Some(old) = old {
            remove_file(ctx.app(), &old).await;
        }
        self.image = Some(image);
        Ok(())
    }

    /// `wire:click="remove"`: delete the image.
    #[guard(auth)]
    pub async fn remove(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        let db = ctx.db()?;
        let post = Post::find_or_404(&db, self.post_id).await?;
        if let Some(old) = post.image.clone() {
            post.update(&db, |m| m.image = Set(None)).await?;
            remove_file(ctx.app(), &old).await;
        }
        self.image = None;
        Ok(())
    }
}

/// Whether `bytes` start like a PNG, JPEG, GIF or WebP file (the name alone proves nothing).
pub fn is_image(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || (bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
}

/// Deletes `storage/app/public/<image>`; a missing file is fine. `image` comes from the database, written by
/// `save`, so it is one of our own `posts/<random>.<ext>` names.
pub async fn remove_file(app: &App, image: &str) {
    if image.contains("..") {
        return;
    }
    let path = app.settings().storage_dir().join("app/public").join(image);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(image, error = %e, "could not delete a post image"),
    }
}
