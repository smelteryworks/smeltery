//! The `posts` resource: list, show, create, edit and delete `Post` records.
//!
//! In `routes/web.rs`, `index` and `show` are public and the other actions sit behind the `auth` middleware: only
//! signed-in users create, edit and delete records. Any signed-in user may change any record; where records belong
//! to someone, check that in the actions.

use smeltery::db::prelude::*;
use smeltery::http::Redirect;
use smeltery::session::Session;
use smeltery::validation::Valid;
use smeltery::{App, Result, Validate};

use crate::app::models::{Post, post};

/// The fields of the create and edit forms, with their validation rules.
#[derive(Debug, Deserialize, Validate)]
pub struct PostForm {}

/// The list page (`resources/views/posts/index.mold.html`).
#[derive(smeltery::Mold)]
#[mold("posts/index")]
pub struct IndexView {
    pub posts: Vec<Post>,
}

/// The create form (`resources/views/posts/create.mold.html`).
#[derive(smeltery::Mold)]
#[mold("posts/create")]
pub struct CreateView {}

/// One record (`resources/views/posts/show.mold.html`).
#[derive(smeltery::Mold)]
#[mold("posts/show")]
pub struct ShowView {
    pub post: Post,
}

/// The edit form (`resources/views/posts/edit.mold.html`).
#[derive(smeltery::Mold)]
#[mold("posts/edit")]
pub struct EditView {
    pub post: Post,
}

/// `GET /posts`: every record.
pub async fn index(db: Db) -> Result<IndexView> {
    Ok(IndexView {
        posts: Post::all(&db).await?,
    })
}

/// `GET /posts/create`: the create form.
pub async fn create() -> CreateView {
    CreateView {}
}

/// `POST /posts`: saves a new record.
pub async fn store(
    app: App,
    db: Db,
    session: Session,
    Valid(_form): Valid<PostForm>,
) -> Result<Redirect> {
    Post::create(
        &db,
        post::ActiveModel {
            ..Default::default()
        },
    )
    .await?;
    session.flash("status", "Post created.");
    Ok(Redirect::to(&app.url("posts.index", &[])?))
}

/// `GET /posts/{post}`: one record.
pub async fn show(Found(post): Found<Post>) -> ShowView {
    ShowView { post }
}

/// `GET /posts/{post}/edit`: the edit form.
pub async fn edit(Found(post): Found<Post>) -> EditView {
    EditView { post }
}

/// `PUT /posts/{post}`: saves the changes.
pub async fn update(
    app: App,
    db: Db,
    session: Session,
    Found(post): Found<Post>,
    Valid(_form): Valid<PostForm>,
) -> Result<Redirect> {
    let post = post.update(&db, |_m| {}).await?;
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
