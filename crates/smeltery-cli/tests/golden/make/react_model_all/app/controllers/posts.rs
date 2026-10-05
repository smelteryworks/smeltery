//! The `posts` resource: list, show, create, edit and delete `Post` records, as React pages.
//!
//! In `routes/web.rs`, `index` and `show` are public and the other actions sit behind the `auth` middleware: only
//! signed-in users create, edit and delete records. Any signed-in user may change any record; where records belong
//! to someone, check that in the actions.
//!
//! `index`, `show` and `edit` send every field of the records to the browser as props, where anyone can read them:
//! when the model holds something private, send a struct with only the public fields instead.

use smeltery::alloy::{self, Page};
use smeltery::db::prelude::*;
use smeltery::http::Redirect;
use smeltery::session::Session;
use smeltery::validation::Valid;
use smeltery::{App, Result, Validate};

use crate::app::models::{Post, post};

/// The fields of the create and edit forms, with their validation rules.
#[derive(Debug, Deserialize, Validate)]
pub struct PostForm {
    #[validate(required, max = 255)]
    pub title: String,
    pub body: Option<String>,
    #[validate(required, integer)]
    pub views: i32,
    #[validate(numeric)]
    pub rating: Option<f64>,
    #[serde(default)]
    pub published: bool,
}

/// `GET /posts`: every record (`resources/js/pages/posts/index.tsx`).
pub async fn index(db: Db) -> Result<Page> {
    let posts = Post::all(&db).await?;
    Ok(alloy::render("posts/index").with("posts", posts))
}

/// `GET /posts/create`: the create form (`resources/js/pages/posts/create.tsx`).
pub async fn create() -> Page {
    alloy::render("posts/create")
}

/// `POST /posts`: saves a new record.
pub async fn store(
    app: App,
    db: Db,
    session: Session,
    Valid(form): Valid<PostForm>,
) -> Result<Redirect> {
    Post::create(
        &db,
        post::ActiveModel {
            title: Set(form.title),
            body: Set(form.body),
            views: Set(form.views),
            rating: Set(form.rating),
            published: Set(form.published),
            ..Default::default()
        },
    )
    .await?;
    session.flash("status", "Post created.");
    Ok(Redirect::to(&app.url("posts.index", &[])?))
}

/// `GET /posts/{post}`: one record (`resources/js/pages/posts/show.tsx`).
pub async fn show(Found(post): Found<Post>) -> Page {
    alloy::render("posts/show").with("post", post)
}

/// `GET /posts/{post}/edit`: the edit form (`resources/js/pages/posts/edit.tsx`).
pub async fn edit(Found(post): Found<Post>) -> Page {
    alloy::render("posts/edit").with("post", post)
}

/// `PUT /posts/{post}`: saves the changes.
pub async fn update(
    app: App,
    db: Db,
    session: Session,
    Found(post): Found<Post>,
    Valid(form): Valid<PostForm>,
) -> Result<Redirect> {
    let post = post
        .update(&db, |m| {
            m.title = Set(form.title);
            m.body = Set(form.body);
            m.views = Set(form.views);
            m.rating = Set(form.rating);
            m.published = Set(form.published);
        })
        .await?;
    session.flash("status", "Post updated.");
    let id = post.id.to_string();
    let url = app.url("posts.show", &[("post", &id)])?;
    Ok(Redirect::to(&url))
}

/// `DELETE /posts/{post}`: deletes the record.
pub async fn destroy(
    app: App,
    db: Db,
    session: Session,
    Found(post): Found<Post>,
) -> Result<Redirect> {
    post.delete(&db).await?;
    session.flash("status", "Post deleted.");
    Ok(Redirect::to(&app.url("posts.index", &[])?))
}
