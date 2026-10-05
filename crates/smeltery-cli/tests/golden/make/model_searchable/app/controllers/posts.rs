//! The `posts` resource: list, show, create, edit and delete `Post` records.
//!
//! In `routes/web.rs`, `index` and `show` are public and the other actions sit behind the `auth` middleware: only
//! signed-in users create, edit and delete records. Any signed-in user may change any record; where records belong
//! to someone, check that in the actions.

use smeltery::db::prelude::*;
use smeltery::http::{Query, Redirect};
use smeltery::prospect::{Prospect, Searchable as _, Segment};
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
    #[validate(integer)]
    pub user_id: Option<i64>,
}

/// The query string of the list: `?q=` (the search text; empty lists every record), with `?page=` read by
/// `PageQuery`.
#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
}

/// The list page (`resources/views/posts/index.mold.html`): the search text and one page of results.
#[derive(smeltery::Mold)]
#[mold("posts/index")]
pub struct IndexView {
    pub q: String,
    pub posts: smeltery::db::Page<PostRow>,
}

/// One result of the list: the record, and its `title` as highlighted parts.
/// The parts are empty when the search has no terms (the template shows the plain text then).
#[derive(Debug, Serialize)]
pub struct PostRow {
    pub post: Post,
    pub label: Vec<Segment>,
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

/// `GET /posts?q=…&page=…`: one page of records matching the search, ranked (highlighted: `title`).
/// It searches every record, as the list shows every record: add `.within(…)` / `.where_eq(…)` to the search when
/// records are private.
pub async fn index(
    prospect: Prospect,
    Query(search): Query<SearchQuery>,
    page: PageQuery,
) -> Result<IndexView> {
    // The text as the search uses it (at most `PROSPECT_MAX_QUERY_LENGTH` characters), also shown in the search box.
    let q: String = search
        .q
        .chars()
        .take(prospect.settings().query_length())
        .collect();
    let hits = Post::search(&prospect, &q)
        .highlight(["title"])
        .paginate(page)
        .await?;
    Ok(IndexView {
        q,
        posts: hits.map(|hit| PostRow {
            label: hit
                .highlights
                .get("title")
                .map(|h| h.segments().to_vec())
                .unwrap_or_default(),
            post: hit.into_model(),
        }),
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
            user_id: Set(form.user_id),
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
            m.user_id = Set(form.user_id);
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
