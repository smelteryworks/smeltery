//! The `posts` resource: list, show, create, edit and delete `Post` records.

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
    #[validate(required)]
    pub body: String,
}

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
        posts: Post::query()
            .order_by_desc(post::Column::Id)
            .all(db.conn())
            .await?,
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
    Valid(form): Valid<PostForm>,
) -> Result<Redirect> {
    Post::create(
        &db,
        post::ActiveModel {
            title: Set(form.title),
            body: Set(form.body),
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
    Valid(form): Valid<PostForm>,
) -> Result<Redirect> {
    let post = post
        .update(&db, |m| {
            m.title = Set(form.title);
            m.body = Set(form.body);
        })
        .await?;
    session.flash("status", "Post updated.");
    let id = post.id.to_string();
    Ok(Redirect::to(&app.url("posts.show", &[("post", &id)])?))
}

/// `DELETE /posts/{post}`: deletes the record.
pub async fn destroy(
    app: App,
    db: Db,
    session: Session,
    Found(post): Found<Post>,
) -> Result<Redirect> {
    let image = post.image.clone();
    post.delete(&db).await?;
    if let Some(image) = image {
        crate::app::sparks::post_image::remove_file(&app, &image).await;
    }
    session.flash("status", "Post deleted.");
    Ok(Redirect::to(&app.url("posts.index", &[])?))
}
