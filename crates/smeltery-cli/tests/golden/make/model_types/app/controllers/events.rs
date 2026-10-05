//! The `events` resource: list, show, create, edit and delete `Event` records.
//!
//! In `routes/web.rs`, `index` and `show` are public and the other actions sit behind the `auth` middleware: only
//! signed-in users create, edit and delete records. Any signed-in user may change any record; where records belong
//! to someone, check that in the actions.

use smeltery::db::prelude::*;
use smeltery::http::Redirect;
use smeltery::session::Session;
use smeltery::validation::Valid;
use smeltery::{App, Result, Validate};

use crate::app::models::{Event, event};

/// The fields of the create and edit forms, with their validation rules.
#[derive(Debug, Deserialize, Validate)]
pub struct EventForm {
    #[validate(required, max = 255)]
    pub name: String,
    pub notes: Option<String>,
    #[validate(required, integer)]
    pub seats: i32,
    #[validate(integer)]
    pub views: Option<i64>,
    #[serde(default)]
    pub open: bool,
    #[validate(numeric)]
    pub price: Option<f64>,
    #[validate(required, integer)]
    pub user_id: i64,
}

/// The list page (`resources/views/events/index.mold.html`).
#[derive(smeltery::Mold)]
#[mold("events/index")]
pub struct IndexView {
    pub events: Vec<Event>,
}

/// The create form (`resources/views/events/create.mold.html`).
#[derive(smeltery::Mold)]
#[mold("events/create")]
pub struct CreateView {}

/// One record (`resources/views/events/show.mold.html`).
#[derive(smeltery::Mold)]
#[mold("events/show")]
pub struct ShowView {
    pub event: Event,
}

/// The edit form (`resources/views/events/edit.mold.html`).
#[derive(smeltery::Mold)]
#[mold("events/edit")]
pub struct EditView {
    pub event: Event,
}

/// `GET /events`: every record.
pub async fn index(db: Db) -> Result<IndexView> {
    Ok(IndexView {
        events: Event::all(&db).await?,
    })
}

/// `GET /events/create`: the create form.
pub async fn create() -> CreateView {
    CreateView {}
}

/// `POST /events`: saves a new record.
pub async fn store(
    app: App,
    db: Db,
    session: Session,
    Valid(form): Valid<EventForm>,
) -> Result<Redirect> {
    Event::create(
        &db,
        event::ActiveModel {
            name: Set(form.name),
            notes: Set(form.notes),
            seats: Set(form.seats),
            views: Set(form.views),
            open: Set(form.open),
            price: Set(form.price),
            user_id: Set(form.user_id),
            ..Default::default()
        },
    )
    .await?;
    session.flash("status", "Event created.");
    Ok(Redirect::to(&app.url("events.index", &[])?))
}

/// `GET /events/{event}`: one record.
pub async fn show(Found(event): Found<Event>) -> ShowView {
    ShowView { event }
}

/// `GET /events/{event}/edit`: the edit form.
pub async fn edit(Found(event): Found<Event>) -> EditView {
    EditView { event }
}

/// `PUT /events/{event}`: saves the changes.
pub async fn update(
    app: App,
    db: Db,
    session: Session,
    Found(event): Found<Event>,
    Valid(form): Valid<EventForm>,
) -> Result<Redirect> {
    let event = event
        .update(&db, |m| {
            m.name = Set(form.name);
            m.notes = Set(form.notes);
            m.seats = Set(form.seats);
            m.views = Set(form.views);
            m.open = Set(form.open);
            m.price = Set(form.price);
            m.user_id = Set(form.user_id);
        })
        .await?;
    session.flash("status", "Event updated.");
    let id = event.id.to_string();
    let url = app.url("events.show", &[("event", &id)])?;
    Ok(Redirect::to(&url))
}

/// `DELETE /events/{event}`: deletes the record.
pub async fn destroy(
    app: App,
    db: Db,
    session: Session,
    Found(event): Found<Event>,
) -> Result<Redirect> {
    event.delete(&db).await?;
    session.flash("status", "Event deleted.");
    Ok(Redirect::to(&app.url("events.index", &[])?))
}
