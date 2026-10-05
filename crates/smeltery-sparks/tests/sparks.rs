//! Sparks through the whole HTTP stack: first render, updates, guards, CSRF, validation, effects, nesting,
//! uploads, the client runtime route and both render modes.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;
use smeltery_core::testing::TestApp;
use smeltery_core::view::view;
use smeltery_core::{AppBuilder, Response, Result};
use smeltery_macros::{Spark, Validate, actions};
use smeltery_mold::{Engine, NoHost, Template};
use smeltery_mold_macros::Mold;
use smeltery_sparks::testing::{TestSpark, post_update};
use smeltery_sparks::{SparkCtx, Sparks, SparksExt, TemporaryUpload};

// ---------------------------------------------------------------- components

#[derive(Debug, Serialize, Deserialize, Default, Spark)]
#[spark(
    name = "counter",
    crate = "smeltery_sparks",
    dir = "tests/app/resources/views"
)]
pub struct Counter {
    pub count: i64,
    #[spark(model)]
    pub step: i64,
}

#[actions(crate = "smeltery_sparks")]
impl Counter {
    pub async fn mount(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        self.count = ctx.prop("start").unwrap_or(0);
        self.step = 1;
        Ok(())
    }

    pub async fn increment(&mut self) -> Result<()> {
        self.count += self.step;
        Ok(())
    }

    pub async fn add(&mut self, ctx: &mut SparkCtx, n: i64) -> Result<()> {
        self.count += n;
        ctx.dispatch("added", json!({ "n": n }));
        Ok(())
    }

    #[guard(auth)]
    pub async fn reset(&mut self) -> Result<()> {
        self.count = 0;
        Ok(())
    }

    pub async fn go(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        ctx.flash("status", "Went");
        ctx.redirect("/done");
        Ok(())
    }

    /// A redirect to a URL that came from data.
    pub async fn go_to(&mut self, ctx: &mut SparkCtx, url: String) -> Result<()> {
        ctx.redirect(url);
        Ok(())
    }

    pub async fn leave(&mut self, ctx: &mut SparkCtx, url: String) -> Result<()> {
        ctx.redirect_away(url);
        Ok(())
    }

    /// Not public: not callable from the page.
    #[allow(dead_code)]
    async fn secret(&mut self) -> Result<()> {
        self.count = 999;
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Spark, Validate)]
#[spark(crate = "smeltery_sparks", dir = "tests/app/resources/views")]
#[validate(crate = "smeltery_core::validation")]
pub struct Signup {
    #[spark(model)]
    #[validate(required, max = 5)]
    pub name: String,
}

#[actions(crate = "smeltery_sparks")]
impl Signup {
    pub async fn save(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        ctx.validate(self).await?;
        ctx.flash("status", "Saved");
        ctx.dispatch("saved", json!({ "name": self.name }));
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Spark)]
#[spark(crate = "smeltery_sparks", dir = "tests/app/resources/views")]
pub struct ItemList {
    pub items: Vec<i64>,
}

#[actions(crate = "smeltery_sparks")]
impl ItemList {
    pub async fn mount(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        // A hook that really waits: it is driven on the render's blocking thread.
        tokio::task::yield_now().await;
        let count: i64 = ctx.prop("count").unwrap_or(0);
        self.items = (1..=count).collect();
        Ok(())
    }

    pub async fn add(&mut self) -> Result<()> {
        let next = self.items.len() as i64 + 1;
        self.items.push(next);
        Ok(())
    }

    pub async fn drop_first(&mut self) -> Result<()> {
        self.items.remove(0);
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Spark)]
#[spark(crate = "smeltery_sparks", dir = "tests/app/resources/views")]
pub struct Item {
    pub n: i64,
}

#[actions(crate = "smeltery_sparks")]
impl Item {
    pub async fn inc(&mut self) -> Result<()> {
        self.n += 1;
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Spark, Validate)]
#[spark(crate = "smeltery_sparks", dir = "tests/app/resources/views")]
#[validate(crate = "smeltery_core::validation")]
pub struct Avatar {
    #[spark(upload(max = 1, mimes = "png"))]
    #[validate(required, min = 0.005)]
    pub photo: Option<TemporaryUpload>,
    pub stored: Option<String>,
    /// Any file type: no `mimes`.
    #[spark(upload(max = 1))]
    pub doc: Option<TemporaryUpload>,
}

#[actions(crate = "smeltery_sparks")]
impl Avatar {
    pub async fn save(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        if let Some(photo) = self.photo.take() {
            self.stored = Some(photo.store(ctx, "public/avatars").await?);
        }
        Ok(())
    }

    /// `save` behind the field's validation rules.
    pub async fn save_checked(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        ctx.validate(self).await?;
        self.save(ctx).await
    }

    pub async fn save_doc(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        if let Some(doc) = self.doc.take() {
            self.stored = Some(doc.store(ctx, "public/docs").await?);
        }
        Ok(())
    }

    pub async fn save_to(&mut self, ctx: &mut SparkCtx, dir: String, file: String) -> Result<()> {
        if let Some(photo) = &self.photo {
            self.stored = Some(photo.store_as(ctx, &dir, &file).await?);
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Spark)]
#[spark(crate = "smeltery_sparks", dir = "tests/app/resources/views", stream)]
pub struct Live {
    pub n: i64,
}

#[actions(crate = "smeltery_sparks")]
impl Live {}

#[derive(Debug, Serialize, Deserialize, Default, Clone, PartialEq)]
pub struct PostForm {
    pub title: String,
    pub author_id: i64,
}

/// A form object: the page may set `form.title` only; `meta` is a plain model field holding a struct.
#[derive(Debug, Serialize, Deserialize, Default, Spark)]
#[spark(crate = "smeltery_sparks", dir = "tests/app/resources/views")]
pub struct Editor {
    #[spark(model(fields = "title"))]
    pub form: PostForm,
    #[spark(model)]
    pub meta: PostForm,
    #[spark(model)]
    pub tags: Vec<String>,
    #[spark(model)]
    pub maybe: Option<PostForm>,
}

#[actions(crate = "smeltery_sparks")]
impl Editor {}

fn register(s: &mut Sparks) {
    s.add::<Counter>()
        .add::<Signup>()
        .add::<ItemList>()
        .add::<Item>()
        .add::<Avatar>()
        .add::<Live>()
        .add::<Editor>()
        .add::<Guarded>();
}

/// Streams only for signed-in visitors: its `can_stream` hook.
#[derive(Debug, Serialize, Deserialize, Default, Spark)]
#[spark(crate = "smeltery_sparks", dir = "tests/app/resources/views", stream)]
pub struct Guarded {
    pub n: i64,
}

#[actions(crate = "smeltery_sparks")]
impl Guarded {
    pub async fn can_stream(&mut self, ctx: &mut SparkCtx) -> Result<bool> {
        Ok(ctx.user_id().is_some())
    }
}

// ---------------------------------------------------------------- pages and app

#[derive(Mold)]
#[mold(
    "pages/counter",
    crate = "smeltery_mold",
    dir = "tests/app/resources/views"
)]
struct CounterPage {}

#[derive(Mold)]
#[mold(
    "pages/all",
    crate = "smeltery_mold",
    dir = "tests/app/resources/views"
)]
struct AllPage {}

#[derive(Mold)]
#[mold(
    "pages/missing",
    crate = "smeltery_mold",
    dir = "tests/app/resources/views"
)]
struct MissingPage {}

async fn counter_page() -> Response {
    view(CounterPage {})
}

async fn all_page() -> Response {
    view(AllPage {})
}

async fn missing_page() -> Response {
    view(MissingPage {})
}

/// A test app with its own root (a temp dir holding a copy of the test views), so uploads land there.
struct Fixture {
    app: TestApp,
    root: PathBuf,
    _dir: tempfile::TempDir,
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn fixture_with(sparks: impl FnOnce(&mut Sparks) + Send + 'static) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app/resources"),
        &root.join("resources"),
    );
    let app_root = root.clone();
    let app = TestApp::new(move |mut b: AppBuilder| {
        b.settings_mut().root = app_root;
        b.settings_mut().debug = true;
        b.sparks(sparks).routes(|r| {
            r.get("/counter", counter_page);
            r.get("/all", all_page);
            r.get("/missing", missing_page);
        })
    });
    Fixture {
        app,
        root,
        _dir: dir,
    }
}

fn fixture() -> Fixture {
    fixture_with(register)
}

fn counter(app: &TestApp) -> TestSpark {
    let page = app.get("/counter");
    assert_eq!(page.status(), 200, "{}", page.text());
    TestSpark::from_html(&page.text(), "counter").expect("the page shows the counter")
}

// ---------------------------------------------------------------- tests

#[test]
fn first_render_then_updates_round_trip() {
    let f = fixture();
    let page = f.app.get("/counter").text();
    assert!(
        page.contains(&format!(
            "<script src=\"/_sparks/sparks.js?v={}\" defer></script>",
            smeltery_sparks::VERSION
        )),
        "{page}"
    );
    assert!(
        page.contains("<meta name=\"csrf-token\" content=\""),
        "{page}"
    );
    assert!(
        page.contains("wire:name=\"counter\" wire:snapshot=\"{&quot;checksum&quot;"),
        "{page}"
    );
    assert!(page.contains("<p>Count: 5</p>"), "{page}");

    let mut c = counter(&f.app);
    assert_eq!(c.data(), json!({ "count": 5, "step": 1 }));
    let id = c.id();
    assert_eq!(id.len(), 20);

    let res = c.call("increment", json!([])).send(&f.app);
    assert_eq!(res.status(), 200, "{}", res.text());
    assert_eq!(c.data()["count"], 6);
    assert!(c.html().contains("<p>Count: 6</p>"), "{}", c.html());
    assert!(
        c.html()
            .starts_with(&format!("<div wire:id=\"{id}\" wire:name=\"counter\""))
    );
    assert_eq!(res.json()["components"][0]["id"], id.as_str());

    // Text from an input is coerced to the field's type.
    c.set("step", "3").call("increment", json!([])).send(&f.app);
    assert_eq!(c.data(), json!({ "count": 9, "step": 3 }));

    // Parameters and dispatched browser events.
    let res = c.call("add", json!([2])).send(&f.app);
    assert_eq!(res.status(), 200);
    assert_eq!(c.data()["count"], 11);
    assert_eq!(
        c.effects(),
        &json!({ "redirect": null, "dispatches": [{ "event": "added", "payload": { "n": 2 } }] })
    );

    // A value that fits no form leaves the field and shows a message.
    let res = c.set("step", "lots").send(&f.app);
    assert_eq!(res.status(), 200);
    assert_eq!(c.data()["step"], 3);
    assert!(
        c.html()
            .contains("<span class=\"err\">The step field is invalid.</span>"),
        "{}",
        c.html()
    );

    // `$refresh` only re-renders.
    assert_eq!(c.call("$refresh", json!([])).send(&f.app).status(), 200);
    assert_eq!(c.data()["count"], 11);
}

#[test]
fn tampered_snapshots_and_versions_answer_419() {
    let f = fixture();
    let c = counter(&f.app);

    let mut tampered = c.clone();
    tampered.set_snapshot(c.snapshot().replace("\"count\":5", "\"count\":500"));
    assert_ne!(tampered.snapshot(), c.snapshot());
    let res = tampered.call("increment", json!([])).send(&f.app);
    assert_eq!(res.status(), 419);
    assert_eq!(res.json()["error"], "Page Expired");

    let mut wrong_version = c.clone();
    wrong_version.set_snapshot(c.snapshot().replace("\"v\":3", "\"v\":2"));
    assert_ne!(wrong_version.snapshot(), c.snapshot());
    assert_eq!(wrong_version.send(&f.app).status(), 419);

    let mut garbage = c.clone();
    garbage.set_snapshot("not json");
    assert_eq!(garbage.send(&f.app).status(), 419);

    let mut body = c.request_body();
    body["v"] = json!(1);
    assert_eq!(post_update(&f.app, &body, None).status(), 419);

    assert_eq!(
        post_update(&f.app, &json!({ "nope": 1 }), None).status(),
        400
    );

    // A snapshot signed under another APP_KEY.
    let other = TestApp::new(|mut b: AppBuilder| {
        b.settings_mut().key = "another-key-another-key-another-key!".into();
        b.settings_mut().root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app");
        b.sparks(register).routes(|r| {
            r.get("/counter", counter_page);
        })
    });
    let mut foreign = counter(&other);
    assert_eq!(foreign.send(&other).status(), 200);
    assert_eq!(foreign.send(&f.app).status(), 419);
}

#[test]
fn only_actions_and_model_fields_are_reachable() {
    let f = fixture();
    let mut c = counter(&f.app);
    for method in ["secret", "mount", "updated", "call", "render_view", "nope"] {
        let res = c.call(method, json!([])).send(&f.app);
        assert_eq!(res.status(), 403, "{method}: {}", res.text());
    }
    // A forbidden call in a batch stops the whole batch: nothing before it ran.
    let res = c
        .call("increment", json!([]))
        .call("secret", json!([]))
        .send(&f.app);
    assert_eq!(res.status(), 403);
    let res = c
        .set("count", 100)
        .call("increment", json!([]))
        .send(&f.app);
    assert_eq!(res.status(), 403, "count is server-only state");
    assert_eq!(c.data()["count"], 5, "nothing ran");

    assert_eq!(c.call("increment", json!([1])).send(&f.app).status(), 400);
    assert_eq!(c.call("add", json!(["x"])).send(&f.app).status(), 400);
    assert_eq!(c.call("add", json!([])).send(&f.app).status(), 400);
    assert_eq!(c.call("add", json!([1])).send(&f.app).status(), 200);
    assert_eq!(c.data()["count"], 6);

    // An app without this component answers 404 for its (validly signed) snapshot.
    let other = fixture_with(|s| {
        s.add::<Live>();
    });
    let mut c = counter(&f.app);
    assert_eq!(c.send(&other.app).status(), 404);
}

#[test]
fn auth_guard_needs_a_signed_in_user() {
    let f = fixture();
    let mut c = counter(&f.app);
    let res = c.call("reset", json!([])).send(&f.app);
    assert_eq!(res.status(), 401);
    assert_eq!(c.data()["count"], 5);
    f.app.acting_as(1);
    // The snapshot was issued to a guest: after signing in it is refused (the page reloads).
    assert_eq!(c.call("reset", json!([])).send(&f.app).status(), 419);
    let mut c = counter(&f.app);
    assert_eq!(c.call("reset", json!([])).send(&f.app).status(), 200);
    assert_eq!(c.data()["count"], 0);
}

#[test]
fn updates_need_the_csrf_token() {
    let f = fixture();
    let app = TestApp::new({
        let root = f.root.clone();
        move |mut b: AppBuilder| {
            b.settings_mut().root = root;
            b.sparks(register).routes(|r| {
                r.get("/counter", counter_page);
            })
        }
    })
    .with_csrf();
    let mut c = counter(&app);
    let without = post_update(&app, &c.call("increment", json!([])).request_body(), None);
    assert_eq!(without.status(), 419);
    let wrong = post_update(&app, &c.request_body(), Some("wrong-token"));
    assert_eq!(wrong.status(), 419);
    // TestSpark sends the token from the page's csrf-token meta tag.
    let res = c.send(&app);
    assert_eq!(res.status(), 200, "{}", res.text());
    assert_eq!(c.data()["count"], 6);
}

#[test]
fn validation_errors_render_inside_the_component() {
    let f = fixture();
    let page = f.app.get("/all").text();
    let mut s = TestSpark::from_html(&page, "signup").unwrap();
    let res = s.call("save", json!([])).send(&f.app);
    assert_eq!(res.status(), 200, "{}", res.text());
    assert!(
        s.html()
            .contains("<span class=\"err\">The name field is required.</span>"),
        "{}",
        s.html()
    );
    assert_eq!(s.effects()["dispatches"], json!([]));

    s.set("name", "Bartholomew")
        .call("save", json!([]))
        .send(&f.app);
    assert!(
        s.html().contains("must not be greater than 5"),
        "{}",
        s.html()
    );
    assert_eq!(s.data()["name"], "Bartholomew", "the update itself stays");

    s.set("name", "Bob").call("save", json!([])).send(&f.app);
    assert!(!s.html().contains("class=\"err\""), "{}", s.html());
    assert!(
        s.html().contains("<p class=\"status\">Saved</p>"),
        "{}",
        s.html()
    );
    assert_eq!(
        s.effects()["dispatches"],
        json!([{ "event": "saved", "payload": { "name": "Bob" } }])
    );
}

#[test]
fn redirect_and_flash_effects() {
    let f = fixture();
    let mut c = counter(&f.app);
    let res = c.call("go", json!([])).send(&f.app);
    assert_eq!(res.status(), 200);
    assert_eq!(c.effects()["redirect"], "/done");
    // The flash shows on the next page.
    let page = f.app.get("/all").text();
    assert!(page.contains("<p class=\"status\">Went</p>"), "{page}");
    let page = f.app.get("/all").text();
    assert!(
        page.contains("<p class=\"status\"></p>"),
        "flash is gone: {page}"
    );
}

#[test]
fn nested_components_keep_their_children() {
    let f = fixture();
    let page = f.app.get("/all").text();
    assert_eq!(page.matches("wire:name=\"item\"").count(), 2, "{page}");
    let mut list = TestSpark::from_html(&page, "item_list").unwrap();
    assert_eq!(list.data()["items"], json!([1, 2]));
    let snapshot: serde_json::Value = serde_json::from_str(list.snapshot()).unwrap();
    let children = snapshot["memo"]["children"].as_object().unwrap().clone();
    assert_eq!(children.len(), 2);
    let first_child = children["1"][0].as_str().unwrap().to_owned();
    assert_eq!(children["1"][1], "item");

    // A child updates on its own.
    let mut item = TestSpark::from_html(&page, "item").unwrap();
    assert_eq!(item.id(), first_child);
    item.call("inc", json!([])).send(&f.app);
    assert!(
        item.html().contains("<span>Item 2</span>"),
        "{}",
        item.html()
    );

    // The parent re-renders: existing children become placeholders, a new key mounts a new child.
    list.call("add", json!([])).send(&f.app);
    let html = list.html();
    assert!(
        html.contains(&format!("<li><div wire:id=\"{first_child}\"></div></li>")),
        "{html}"
    );
    assert_eq!(
        html.matches("<div wire:id=\"").count(),
        4,
        "root, 2 placeholders, 1 new child: {html}"
    );
    assert!(html.contains("<span>Item 3</span>"), "{html}");
    let snapshot: serde_json::Value = serde_json::from_str(list.snapshot()).unwrap();
    assert_eq!(snapshot["memo"]["children"].as_object().unwrap().len(), 3);

    // A child left out of the render is forgotten.
    list.call("drop_first", json!([])).send(&f.app);
    let snapshot: serde_json::Value = serde_json::from_str(list.snapshot()).unwrap();
    let children = snapshot["memo"]["children"].as_object().unwrap();
    assert!(!children.contains_key("1"));
    assert_eq!(children.len(), 2);
}

fn avatar(f: &Fixture) -> TestSpark {
    let page = f.app.get("/all").text();
    TestSpark::from_html(&page, "avatar").unwrap()
}

fn token(res: &smeltery_core::testing::TestResponse) -> String {
    assert_eq!(res.status(), 200, "{}", res.text());
    res.json()["token"].as_str().unwrap().to_owned()
}

fn tmp_files(root: &Path) -> usize {
    std::fs::read_dir(root.join("storage/framework/sparks"))
        .map(|d| d.count())
        .unwrap_or(0)
}

#[test]
fn uploads_are_validated_and_stored() {
    let f = fixture();
    let mut a = avatar(&f);
    let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3];
    let t = token(&a.upload(&f.app, "photo", "../../me.png", "image/png", png.clone()));
    assert_eq!(tmp_files(&f.root), 1);
    let res = a.set("photo", &t).send(&f.app);
    assert_eq!(res.status(), 200, "{}", res.text());
    assert_eq!(
        a.data()["photo"]["name"],
        "me.png",
        "directories are dropped from the name"
    );
    assert_eq!(a.data()["photo"]["size"], 7);
    assert_eq!(a.data()["photo"]["mime"], "image/png");

    a.call("save", json!([])).send(&f.app);
    let stored = a.data()["stored"].as_str().unwrap().to_owned();
    assert!(
        stored.starts_with("public/avatars/") && stored.ends_with(".png"),
        "{stored}"
    );
    assert_eq!(stored.len(), "public/avatars/".len() + 40 + 4);
    assert_eq!(
        std::fs::read(f.root.join("storage/app").join(&stored)).unwrap(),
        png
    );
    assert_eq!(tmp_files(&f.root), 0, "the temp file moved");
    assert!(a.data()["photo"].is_null());
    assert!(!f.root.join("public").exists(), "nothing lands in public/");
}

#[test]
fn upload_rules_become_validation_messages() {
    let f = fixture();
    let mut a = avatar(&f);
    let big = token(&a.upload(&f.app, "photo", "big.png", "image/png", vec![0; 1025]));
    assert_eq!(tmp_files(&f.root), 0, "a too large file is removed");
    a.set("photo", &big).send(&f.app);
    assert!(
        a.html()
            .contains("The photo must not be greater than 1 kilobytes."),
        "{}",
        a.html()
    );
    assert!(a.data()["photo"].is_null());

    let gif = token(&a.upload(&f.app, "photo", "x.gif", "image/gif", vec![1]));
    a.set("photo", &gif).send(&f.app);
    assert!(
        a.html().contains("The photo must be a file of type: png."),
        "{}",
        a.html()
    );

    // A field that is not an upload field, or another component's field.
    assert_eq!(
        a.upload(&f.app, "stored", "x.png", "image/png", vec![1])
            .status(),
        403
    );
    let c = counter(&f.app);
    assert_eq!(
        c.upload(&f.app, "photo", "x.png", "image/png", vec![1])
            .status(),
        403
    );

    // `null` clears the field.
    let ok = token(&a.upload(&f.app, "photo", "x.png", "image/png", vec![1]));
    a.set("photo", &ok).send(&f.app);
    assert!(!a.data()["photo"].is_null());
    a.set("photo", serde_json::Value::Null).send(&f.app);
    assert!(a.data()["photo"].is_null());
}

#[test]
fn upload_tokens_are_signed_bound_and_expire() {
    let f = fixture();
    let mut a = avatar(&f);
    let t = token(&a.upload(&f.app, "photo", "x.png", "image/png", vec![1]));

    // Tampered: another body under the same signature.
    let (body, sig) = t.split_once('.').unwrap();
    let mut forged = body.to_owned();
    forged.replace_range(0..1, if body.starts_with('e') { "f" } else { "e" });
    assert_eq!(
        a.set("photo", format!("{forged}.{sig}"))
            .send(&f.app)
            .status(),
        403
    );
    assert_eq!(a.set("photo", "garbage").send(&f.app).status(), 403);
    assert_eq!(a.set("photo", 5).send(&f.app).status(), 403);

    // Another visitor (a new session) cannot use the token.
    f.app.clear_cookies();
    let mut b = avatar(&f);
    assert_eq!(b.set("photo", &t).send(&f.app).status(), 403);

    // Expired tokens become a message.
    let expiring = fixture_with(|s| {
        register(s);
        s.upload_ttl(std::time::Duration::ZERO);
    });
    let mut a = avatar(&expiring);
    let t = token(&a.upload(&expiring.app, "photo", "x.png", "image/png", vec![1]));
    a.set("photo", &t).send(&expiring.app);
    assert!(
        a.html().contains("The photo upload has expired."),
        "{}",
        a.html()
    );
}

#[test]
fn storing_rejects_paths_outside_storage() {
    let f = fixture();
    let mut a = avatar(&f);
    let t = token(&a.upload(&f.app, "photo", "x.png", "image/png", vec![1, 2]));
    a.set("photo", &t).send(&f.app);
    for (dir, file) in [
        ("public/../../etc", "x.png"),
        ("../public", "x.png"),
        ("/tmp", "x.png"),
        ("other", "x.png"),
        ("public", "../x.png"),
        ("public", ".htaccess"),
        ("public", "a/b.png"),
    ] {
        let res = a.call("save_to", json!([dir, file])).send(&f.app);
        assert_eq!(res.status(), 400, "{dir} {file}: {}", res.text());
    }
    assert_eq!(tmp_files(&f.root), 1, "nothing moved");
    let res = a
        .call("save_to", json!(["private/docs", "kept.png"]))
        .send(&f.app);
    assert_eq!(res.status(), 200, "{}", res.text());
    assert_eq!(a.data()["stored"], "private/docs/kept.png");
    assert!(f.root.join("storage/app/private/docs/kept.png").exists());
}

#[test]
fn the_client_runtime_is_served_with_long_caching() {
    let f = fixture();
    let res = f.app.get(&format!(
        "/_sparks/sparks.js?v={}",
        smeltery_sparks::VERSION
    ));
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.header("content-type"),
        Some("application/javascript; charset=utf-8")
    );
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(res.text(), smeltery_sparks::SPARKS_JS);
    assert!(
        res.header("set-cookie").is_none(),
        "no session for the script"
    );
}

#[test]
fn unknown_components_are_template_errors() {
    let f = fixture();
    let res = f.app.get("/missing");
    assert_eq!(res.status(), 500);
    assert!(
        res.text().contains("unknown Spark `missing`"),
        "{}",
        res.text()
    );
}

#[test]
fn stream_components_are_marked() {
    let f = fixture();
    let page = f.app.get("/all").text();
    assert!(page.contains("wire:name=\"live\" wire:snapshot="), "{page}");
    let live = TestSpark::from_html(&page, "live").unwrap();
    assert!(
        live.html()
            .split('>')
            .next()
            .unwrap()
            .contains(" wire:stream=\""),
        "{}",
        live.html()
    );
    let c = TestSpark::from_html(&page, "signup").unwrap();
    assert!(!c.html().split('>').next().unwrap().contains("wire:stream"));
}

#[test]
fn both_render_modes_produce_the_same_bytes() {
    let engine =
        Engine::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app/resources/views"));
    let counter = Counter { count: 3, step: 2 };
    let runtime = counter.render_runtime_with(&engine, &NoHost).unwrap();
    assert_eq!(runtime, counter.render_compiled(&NoHost).unwrap());
    assert!(runtime.contains("<p>Count: 3</p>"));
    let signup = Signup { name: "A&B".into() };
    assert_eq!(
        signup.render_runtime_with(&engine, &NoHost).unwrap(),
        signup.render_compiled(&NoHost).unwrap()
    );
    let avatar = Avatar {
        photo: None,
        stored: Some("public/a.png".into()),
        doc: None,
    };
    assert_eq!(
        avatar.render_runtime_with(&engine, &NoHost).unwrap(),
        avatar.render_compiled(&NoHost).unwrap()
    );
}

#[tokio::test]
async fn duplicate_names_fail_the_build() {
    let mut settings = smeltery_core::config::Settings::from_env();
    settings.env = "testing".into();
    let err = AppBuilder::new(settings)
        .sparks(|s| {
            s.add::<Counter>().add::<Counter>();
        })
        .build()
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("two Sparks share the name `counter`"),
        "{err}"
    );
}

// ---------------------------------------------------------------- rendering hook + extend

/// Counts its renders in the `rendering` hook; registered with `smeltery_sparks::extend`.
#[derive(Debug, Serialize, Deserialize, Default, Spark)]
#[spark(
    name = "clock",
    crate = "smeltery_sparks",
    dir = "tests/app/resources/views"
)]
pub struct Clock {
    pub renders: i64,
}

#[actions(crate = "smeltery_sparks")]
impl Clock {
    pub async fn rendering(&mut self, _ctx: &mut SparkCtx) -> Result<()> {
        self.renders += 1;
        Ok(())
    }
}

#[derive(Mold)]
#[mold(
    "pages/clock",
    crate = "smeltery_mold",
    dir = "tests/app/resources/views"
)]
struct ClockPage {}

async fn clock_page() -> Response {
    view(ClockPage {})
}

#[test]
fn the_rendering_hook_runs_before_every_render_and_extend_adds_components_at_boot() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app/resources"),
        &root.join("resources"),
    );
    let app = TestApp::new(move |mut b: AppBuilder| {
        b.settings_mut().root = root;
        b.on_boot(|app| async move {
            // Before `.sparks(…)` in the chain: boot hooks run after every builder call.
            assert!(smeltery_sparks::extend(&app, |s| {
                s.add::<Clock>();
            })?);
            // A taken name fails.
            assert!(
                smeltery_sparks::extend(&app, |s| {
                    s.add::<Clock>();
                })
                .is_err()
            );
            Ok(())
        })
        .sparks(|_| {})
        .routes(|r| {
            r.get("/clock", clock_page);
        })
    });
    let page = app.get("/clock");
    assert_eq!(page.status(), 200, "{}", page.text());
    assert!(page.text().contains("renders: 1"));
    let mut clock = TestSpark::from_html(&page.text(), "clock").unwrap();
    assert_eq!(clock.call("$refresh", json!([])).send(&app).status(), 200);
    assert!(clock.html().contains("renders: 2"));
    assert_eq!(clock.data()["renders"], 2);
    // `rendering` is a hook, not an action.
    assert_eq!(clock.call("rendering", json!([])).send(&app).status(), 403);

    // Without Sparks, `extend` reports false.
    let plain = TestApp::new(|b: AppBuilder| {
        b.on_boot(|app| async move {
            assert!(!smeltery_sparks::extend(&app, |s| {
                s.add::<Clock>();
            })?);
            Ok(())
        })
    });
    drop(plain);
}

#[test]
fn the_app_is_freed_after_sparks_ran() {
    let f = fixture();
    let mut c = counter(&f.app);
    assert_eq!(c.call("increment", json!([])).send(&f.app).status(), 200);
    let weak = f.app.app().downgrade();
    drop(f);
    assert!(
        weak.upgrade().is_none(),
        "a Sparks service kept the app alive"
    );
}

#[test]
fn validation_rules_apply_to_upload_fields() {
    let f = fixture();
    let mut a = avatar(&f);
    a.call("save_checked", json!([])).send(&f.app);
    assert!(
        a.html().contains("The photo field is required."),
        "{}",
        a.html()
    );
    // 3 bytes: under the `min = 0.005` kilobytes (5.12 bytes) of the rule.
    let small = token(&a.upload(&f.app, "photo", "x.png", "image/png", vec![1, 2, 3]));
    a.set("photo", &small)
        .call("save_checked", json!([]))
        .send(&f.app);
    assert!(
        a.html()
            .contains("The photo field must be at least 0.005 kilobytes."),
        "{}",
        a.html()
    );
    assert!(a.data()["stored"].is_null());
    let ok = token(&a.upload(&f.app, "photo", "y.png", "image/png", vec![7; 8]));
    a.set("photo", &ok)
        .call("save_checked", json!([]))
        .send(&f.app);
    assert!(a.data()["stored"].as_str().unwrap().ends_with(".png"));
}

// ---------------------------------------------------------------- security limits (D-331..D-337)

/// A snapshot works only for the session and user it was issued to, and only for its time to live (S3-02).
#[test]
fn snapshots_are_bound_to_their_session_and_user_and_expire() {
    let f = fixture();
    let mut c = counter(&f.app);
    assert_eq!(c.call("increment", json!([])).send(&f.app).status(), 200);
    assert_eq!(c.data()["count"], 6, "the same session works");

    // Another browser (a new session) replays the captured snapshot.
    let stolen = c.clone();
    f.app.clear_cookies();
    let _fresh = counter(&f.app);
    let mut replay = stolen.clone();
    let res = replay.call("increment", json!([])).send(&f.app);
    assert_eq!(res.status(), 419, "{}", res.text());

    // Signed in as another user in the same browser: the guest's snapshot is refused too.
    let mut guest = counter(&f.app);
    f.app.acting_as(7);
    assert_eq!(
        guest.call("increment", json!([])).send(&f.app).status(),
        419
    );
    let mut user = counter(&f.app);
    assert_eq!(user.call("increment", json!([])).send(&f.app).status(), 200);
    f.app.acting_as(8);
    assert_eq!(user.call("increment", json!([])).send(&f.app).status(), 419);

    // Past its time to live.
    let short = fixture_with(|s| {
        register(s);
        s.snapshot_ttl(std::time::Duration::ZERO);
    });
    let mut c = counter(&short.app);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert_eq!(
        c.call("increment", json!([])).send(&short.app).status(),
        419
    );
}

/// Requests carry at most `max_components` components and `max_calls` calls each; more answers 413 before
/// anything runs (S3-04).
#[test]
fn oversized_update_requests_are_refused() {
    let f = fixture();
    let c = counter(&f.app);
    let one = json!({ "snapshot": c.snapshot(), "updates": {}, "calls": [{ "method": "increment", "params": [] }] });
    let two =
        json!({ "v": smeltery_sparks::PROTOCOL_VERSION, "components": [one.clone(), one.clone()] });
    let res = post_update(&f.app, &two, None);
    assert_eq!(res.status(), 413, "{}", res.text());

    let calls = |n: usize| {
        let calls: Vec<_> = (0..n)
            .map(|_| json!({ "method": "increment", "params": [] }))
            .collect();
        json!({ "v": smeltery_sparks::PROTOCOL_VERSION, "components": [{ "snapshot": c.snapshot(), "calls": calls }] })
    };
    assert_eq!(post_update(&f.app, &calls(51), None).status(), 413);
    let res = post_update(&f.app, &calls(50), None);
    assert_eq!(res.status(), 200, "{}", res.text());
    let snapshot: serde_json::Value =
        serde_json::from_str(res.json()["components"][0]["snapshot"].as_str().unwrap()).unwrap();
    assert_eq!(snapshot["data"]["count"], 55);

    // Both caps are settings.
    let wide = fixture_with(|s| {
        register(s);
        s.max_components(2).max_calls(1);
    });
    let c = counter(&wide.app);
    let one =
        json!({ "snapshot": c.snapshot(), "calls": [{ "method": "increment", "params": [] }] });
    let body = json!({ "v": smeltery_sparks::PROTOCOL_VERSION, "components": [one.clone(), one] });
    assert_eq!(post_update(&wide.app, &body, None).status(), 200);
    let mut c = counter(&wide.app);
    let res = c
        .call("increment", json!([]))
        .call("increment", json!([]))
        .send(&wide.app);
    assert_eq!(res.status(), 413);
}

/// Several components in one request: all are checked before any runs; one that fails after others ran gets an
/// error entry next to their results (S3-11).
#[test]
fn multi_component_requests_are_checked_first_and_answer_what_ran() {
    let f = fixture_with(|s| {
        register(s);
        s.max_components(2);
    });
    // A forbidden call in the second component: the first (which flashes) never ran.
    let c = counter(&f.app);
    let body = json!({ "v": smeltery_sparks::PROTOCOL_VERSION, "components": [
        { "snapshot": c.snapshot(), "calls": [{ "method": "go", "params": [] }] },
        { "snapshot": c.snapshot(), "calls": [{ "method": "secret", "params": [] }] },
    ] });
    assert_eq!(post_update(&f.app, &body, None).status(), 403);
    let page = f.app.get("/all").text();
    assert!(
        page.contains("<p class=\"status\"></p>"),
        "nothing flashed: {page}"
    );

    // The second component fails while running: the first one's result is answered with its error.
    let c = counter(&f.app);
    let body = json!({ "v": smeltery_sparks::PROTOCOL_VERSION, "components": [
        { "snapshot": c.snapshot(), "calls": [{ "method": "increment", "params": [] }] },
        { "snapshot": c.snapshot(), "calls": [{ "method": "go_to", "params": ["javascript:alert(1)"] }] },
    ] });
    let res = post_update(&f.app, &body, None);
    assert_eq!(res.status(), 200, "{}", res.text());
    let components = res.json()["components"].as_array().unwrap().clone();
    assert_eq!(components.len(), 2);
    let first: serde_json::Value =
        serde_json::from_str(components[0]["snapshot"].as_str().unwrap()).unwrap();
    assert_eq!(first["data"]["count"], 6);
    assert_eq!(components[1]["id"], c.id().as_str());
    assert_eq!(
        components[1]["error"],
        json!({ "status": 500, "message": "Internal Server Error" })
    );
    assert!(components[1].get("snapshot").is_none());
}

/// `wire:model` never sets a struct from an object; a `model(fields = …)` field takes only its listed keys
/// (S3-08).
#[test]
fn struct_model_fields_take_only_listed_keys() {
    let f = fixture();
    let page = f.app.get("/all").text();
    let mut e = TestSpark::from_html(&page, "editor").unwrap();
    // Mass assignment through the whole object is refused.
    let res = e
        .set("form", json!({ "title": "hi", "author_id": 999 }))
        .send(&f.app);
    assert_eq!(res.status(), 403, "{}", res.text());
    assert_eq!(e.set("form.author_id", 999).send(&f.app).status(), 403);
    assert_eq!(
        e.set("meta", json!({ "title": "x" })).send(&f.app).status(),
        403
    );
    assert_eq!(
        e.set("tags", json!([{ "x": 1 }])).send(&f.app).status(),
        403
    );
    assert_eq!(e.data()["form"], json!({ "title": "", "author_id": 0 }));

    // A listed key, alone or as an object of listed keys.
    assert_eq!(e.set("form.title", "Hello").send(&f.app).status(), 200);
    assert_eq!(
        e.data()["form"],
        json!({ "title": "Hello", "author_id": 0 })
    );
    assert!(e.html().contains("value=\"Hello\""), "{}", e.html());
    assert_eq!(
        e.set("form", json!({ "title": "Again" }))
            .send(&f.app)
            .status(),
        200
    );
    assert_eq!(e.data()["form"]["title"], "Again");
    // Lists of plain values stay settable.
    assert_eq!(e.set("tags", json!(["a", "b"])).send(&f.app).status(), 200);
    assert_eq!(e.data()["tags"], json!(["a", "b"]));
}

/// `ctx.redirect` follows only paths on this site and `APP_URL` addresses; `redirect_away` only http(s) URLs
/// (S3-06).
#[test]
fn redirects_go_only_to_safe_targets() {
    let f = fixture();
    let app_url = f.app.app().settings().url.clone();
    let mut c = counter(&f.app);
    for ok in ["/done".to_owned(), format!("{app_url}/x")] {
        let res = c.call("go_to", json!([ok])).send(&f.app);
        assert_eq!(res.status(), 200, "{ok}: {}", res.text());
        assert_eq!(c.effects()["redirect"], ok.as_str());
    }
    for bad in [
        "javascript:alert(1)",
        "JAVASCRIPT:alert(1)",
        "//evil.test/x",
        "/\\evil.test",
        "/\tx",
        "https://evil.test/",
        "data:text/html,x",
        "",
    ] {
        let res = c.call("go_to", json!([bad])).send(&f.app);
        assert_eq!(res.status(), 500, "{bad:?}: {}", res.text());
    }
    let res = c
        .call("leave", json!(["https://example.test/a?b=1"]))
        .send(&f.app);
    assert_eq!(res.status(), 200, "{}", res.text());
    assert_eq!(c.effects()["redirect"], "https://example.test/a?b=1");
    for bad in [
        "javascript:alert(1)",
        "/local",
        "ftp://x.test/",
        "https:// x",
    ] {
        assert_eq!(
            c.call("leave", json!([bad])).send(&f.app).status(),
            500,
            "{bad:?}"
        );
    }
}

/// One session uploads at most the configured files and bytes per window; another session has its own quota
/// (S3-05).
#[test]
fn uploads_have_a_per_session_quota() {
    let f = fixture_with(|s| {
        register(s);
        s.upload_quota(2, 1 << 20, std::time::Duration::from_secs(600));
    });
    let a = avatar(&f);
    for _ in 0..2 {
        token(&a.upload(&f.app, "photo", "x.png", "image/png", vec![1]));
    }
    let res = a.upload(&f.app, "photo", "x.png", "image/png", vec![1]);
    assert_eq!(res.status(), 429, "{}", res.text());
    assert_eq!(tmp_files(&f.root), 2, "the refused upload wrote nothing");
    f.app.clear_cookies();
    let b = avatar(&f);
    token(&b.upload(&f.app, "photo", "x.png", "image/png", vec![1]));

    let bytes = fixture_with(|s| {
        register(s);
        s.upload_quota(100, 10, std::time::Duration::from_secs(600));
    });
    let a = avatar(&bytes);
    token(&a.upload(&bytes.app, "photo", "x.png", "image/png", vec![1; 8]));
    token(&a.upload(&bytes.app, "photo", "x.png", "image/png", vec![1; 8]));
    assert_eq!(
        a.upload(&bytes.app, "photo", "x.png", "image/png", vec![1])
            .status(),
        429
    );
}

/// A JSON array reaches a struct through serde's sequence form: a plain model field never ends up holding a struct,
/// whatever the value's shape (S3-08 re-check).
#[test]
fn struct_model_fields_refuse_arrays_too() {
    let f = fixture();
    let page = f.app.get("/all").text();
    let mut e = TestSpark::from_html(&page, "editor").unwrap();
    assert_eq!(e.set("meta", json!(["hi", 999])).send(&f.app).status(), 403);
    assert_eq!(
        e.set("maybe", json!(["hi", 999])).send(&f.app).status(),
        403
    );
    assert_eq!(e.set("form", json!(["hi", 999])).send(&f.app).status(), 403);
    // Nothing ran: the state is unchanged, and a refused update in a batch stops the batch.
    let res = e
        .set("tags", json!(["a"]))
        .set("meta", json!(["hi", 999]))
        .send(&f.app);
    assert_eq!(res.status(), 403);
    assert_eq!(e.data()["meta"], json!({ "title": "", "author_id": 0 }));
    assert!(e.data()["maybe"].is_null());
    assert_eq!(e.data()["tags"], json!([]));
    // Clearing an optional struct and setting a list of text stay possible.
    assert_eq!(
        e.set("maybe", serde_json::Value::Null)
            .send(&f.app)
            .status(),
        200
    );
    assert_eq!(e.set("tags", json!(["a", "b"])).send(&f.app).status(), 200);
}

/// A fresh session is one page load away, so uploads are also counted per client address (S3-05 re-check).
#[test]
fn uploads_have_a_per_address_quota() {
    let f = fixture_with(|s| {
        register(s);
        s.upload_quota(100, 1 << 20, std::time::Duration::from_secs(600))
            .upload_address_quota(3, 1 << 20);
    });
    f.app.from_addr("203.0.113.7:4000".parse().unwrap());
    let a = avatar(&f);
    for _ in 0..2 {
        token(&a.upload(&f.app, "photo", "x.png", "image/png", vec![1]));
    }
    f.app.clear_cookies();
    let b = avatar(&f);
    token(&b.upload(&f.app, "photo", "x.png", "image/png", vec![1]));
    let res = b.upload(&f.app, "photo", "x.png", "image/png", vec![1]);
    assert_eq!(res.status(), 429, "{}", res.text());
    // Another address has its own count.
    f.app.from_addr("203.0.113.8:4000".parse().unwrap());
    f.app.clear_cookies();
    let c = avatar(&f);
    token(&c.upload(&f.app, "photo", "x.png", "image/png", vec![1]));
}

/// Sweep W6-01: an upload whose request is cancelled (here by `REQUEST_TIMEOUT`, as a closed connection would) leaves
/// no temp file, and the bytes it sent count against the quota. Before the fix the partial file stayed for 24 hours
/// and counted as 0 bytes, so stalled uploads got past the byte quota.
#[test]
fn a_cancelled_upload_leaves_no_file_and_its_bytes_count() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app/resources"),
        &root.join("resources"),
    );
    let app_root = root.clone();
    let app = TestApp::new(move |mut b: AppBuilder| {
        b.settings_mut().root = app_root;
        b.settings_mut().debug = true;
        b.settings_mut().request_timeout = std::time::Duration::from_millis(300);
        b.sparks(|s| {
            register(s);
            s.upload_quota(100, 1 << 20, std::time::Duration::from_secs(600))
                .upload_address_quota(100, 1000);
        })
        .routes(|r| {
            r.get("/all", all_page);
        })
    });
    app.from_addr("203.0.113.9:4000".parse().unwrap());
    // `photo` takes at most 1 KiB: 1023 bytes arrive, then the body stalls until the request times out.
    let stalled = futures_util::StreamExt::chain(
        futures_util::stream::iter([Ok::<_, std::io::Error>(bytes::Bytes::from(vec![0u8; 1023]))]),
        futures_util::stream::pending(),
    );
    let mut headers = http::HeaderMap::new();
    headers.insert(http::header::CONTENT_TYPE, "image/png".parse().unwrap());
    let res = app.request(
        http::Method::POST,
        "/_sparks/upload?component=avatar&field=photo&name=x.png",
        headers,
        axum::body::Body::from_stream(stalled),
    );
    assert_eq!(res.status(), 408, "{}", res.text());
    let tmp = root.join("storage/framework/sparks");
    let left: Vec<_> = std::fs::read_dir(&tmp)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "temp files left behind: {left:?}");
    // The 1023 bytes are over this address's 1000-byte quota: the next upload is refused before it is read.
    let page = app.get("/all");
    let avatar = TestSpark::from_html(&page.text(), "avatar").unwrap();
    let res = avatar.upload(&app, "photo", "x.png", "image/png", vec![1]);
    assert_eq!(res.status(), 429, "{}", res.text());
}

/// A streamed component's render carries a stream token only when its `can_stream` hook allows it, at the first
/// render and on every update (Watchfire I-1: refresh signals of a panel reached guests).
#[test]
fn the_can_stream_hook_decides_who_gets_a_stream_token() {
    let f = fixture();
    let tag = |s: &TestSpark| s.html().split('>').next().unwrap().to_owned();
    let page = f.app.get("/all").text();
    let mut guarded = TestSpark::from_html(&page, "guarded").unwrap();
    assert!(!tag(&guarded).contains("wire:stream"), "{}", tag(&guarded));
    assert_eq!(
        guarded.call("$refresh", json!([])).send(&f.app).status(),
        200
    );
    assert!(!tag(&guarded).contains("wire:stream"), "{}", tag(&guarded));
    let live = TestSpark::from_html(&page, "live").unwrap();
    assert!(tag(&live).contains(" wire:stream=\""), "{}", tag(&live));

    f.app.acting_as(1);
    let page = f.app.get("/all").text();
    let mut guarded = TestSpark::from_html(&page, "guarded").unwrap();
    assert!(
        tag(&guarded).contains(" wire:stream=\""),
        "{}",
        tag(&guarded)
    );
    assert_eq!(
        guarded.call("$refresh", json!([])).send(&f.app).status(),
        200
    );
    assert!(
        tag(&guarded).contains(" wire:stream=\""),
        "{}",
        tag(&guarded)
    );
}

/// Only an allow-listed extension is kept; anything else (`html`, `svg`, `js`, the XML types browsers run script in,
/// an unknown one) is stored as `.bin`, like core's `UploadedFile::store` (S3-03, R-2).
#[test]
fn active_content_uploads_are_stored_as_bin() {
    let f = fixture();
    let mut a = avatar(&f);
    for (name, ext) in [
        ("evil.html", "bin"),
        ("x.SVG", "bin"),
        ("a.js", "bin"),
        ("s.xsd", "bin"),
        ("m.mathml", "bin"),
        ("p.xhtml", "bin"),
        ("feed.rss", "bin"),
        ("odd.zzz", "bin"),
        ("notes.txt", "txt"),
        ("Sheet.CSV", "csv"),
        ("clip.mp4", "mp4"),
    ] {
        let t = token(&a.upload(
            &f.app,
            "doc",
            name,
            "text/html",
            b"<script>1</script>".to_vec(),
        ));
        assert_eq!(a.set("doc", &t).send(&f.app).status(), 200);
        let res = a.call("save_doc", json!([])).send(&f.app);
        assert_eq!(res.status(), 200, "{}", res.text());
        let stored = a.data()["stored"].as_str().unwrap().to_owned();
        assert!(stored.ends_with(&format!(".{ext}")), "{name}: {stored}");
        assert!(f.root.join("storage/app").join(&stored).exists());
    }
}

/// An IPv6 client is counted by its /64: another address of the same network shares the quota (S3-05 re-check 2).
#[test]
fn the_address_quota_counts_an_ipv6_client_by_its_64() {
    let f = fixture_with(|s| {
        register(s);
        s.upload_quota(100, 1 << 20, std::time::Duration::from_secs(600))
            .upload_address_quota(2, 1 << 20);
    });
    for addr in ["[2001:db8:1:2::1]:4000", "[2001:db8:1:2:ffff::9]:4000"] {
        f.app.from_addr(addr.parse().unwrap());
        f.app.clear_cookies();
        let a = avatar(&f);
        token(&a.upload(&f.app, "photo", "x.png", "image/png", vec![1]));
    }
    f.app
        .from_addr("[2001:db8:1:2:abcd::1]:4000".parse().unwrap());
    f.app.clear_cookies();
    let a = avatar(&f);
    assert_eq!(
        a.upload(&f.app, "photo", "x.png", "image/png", vec![1])
            .status(),
        429
    );
    // Another /64 is another client.
    f.app.from_addr("[2001:db8:1:3::1]:4000".parse().unwrap());
    f.app.clear_cookies();
    let b = avatar(&f);
    token(&b.upload(&f.app, "photo", "x.png", "image/png", vec![1]));
}
