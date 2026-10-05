//! The posts resource: CRUD behind `auth`, form validation, and the image upload of the `post_image` Spark.
//! Every test app writes its files under a temporary root, so uploads never land in the real `storage/`.

use std::path::{Path, PathBuf};

use smeltery::db::factory::Factory;
use smeltery::db::prelude::Record as _;
use smeltery::json;
use smeltery::sparks::testing::TestSpark;
use smeltery::testing::TestApp;

use web_demo::app::models::Post;
use web_demo::database::factories::user_factory::UserFactory;

/// A PNG file's first bytes (the signature) plus some data.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-not-a-real-image-but-a-png-signature";

/// A test app whose root is a temp dir holding a copy of `resources/` (views are read from the root in debug
/// builds), signed in as a new user.
struct Demo {
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

fn demo() -> Demo {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("resources"),
        &root.join("resources"),
    );
    let app_root = root.clone();
    let app = TestApp::new(move |mut b| {
        b.settings_mut().root = app_root;
        web_demo::build(b)
    });
    let user = app.block_on(UserFactory.create(&app.db())).unwrap();
    app.acting_as(user.id);
    Demo {
        app,
        root,
        _dir: dir,
    }
}

fn post_count(app: &TestApp) -> u64 {
    app.block_on(Post::count(&app.db())).unwrap()
}

fn only_post(app: &TestApp) -> Post {
    let mut all = app.block_on(Post::all(&app.db())).unwrap();
    assert_eq!(all.len(), 1);
    all.remove(0)
}

fn create_post(app: &TestApp) -> Post {
    let res = app.post_form("/posts", &[("title", "Hello"), ("body", "First post")]);
    assert_eq!(res.status(), 303, "{}", res.text());
    only_post(app)
}

fn image_spark(app: &TestApp, post: &Post) -> TestSpark {
    let page = app.get(&format!("/posts/{}", post.id));
    assert_eq!(page.status(), 200, "{}", page.text());
    TestSpark::from_html(&page.text(), "post_image").expect("the post page shows the image Spark")
}

fn upload_token(spark: &TestSpark, app: &TestApp, name: &str, bytes: Vec<u8>) -> String {
    let res = spark.upload(app, "photo", name, "image/png", bytes);
    assert_eq!(res.status(), 200, "{}", res.text());
    res.json()["token"].as_str().unwrap().to_owned()
}

#[test]
fn guests_cannot_reach_the_posts() {
    let app = TestApp::new(web_demo::build);
    for path in ["/posts", "/posts/create", "/posts/1", "/posts/1/edit"] {
        let res = app.get(path);
        assert_eq!(res.status(), 303, "{path}");
        assert_eq!(res.header("location"), Some("/login"), "{path}");
    }
    let res = app.post_form("/posts", &[("title", "x"), ("body", "y")]);
    assert_eq!(res.status(), 303);
    assert_eq!(post_count(&app), 0);
}

#[test]
fn posts_can_be_created_shown_edited_and_deleted() {
    let d = demo();
    let app = &d.app;
    assert_eq!(app.get("/posts/create").status(), 200);
    assert!(app.get("/posts").text().contains("No posts yet."));

    let res = app.post_form("/posts", &[("title", "Hello"), ("body", "First post")]);
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/posts"));
    let index = app.get("/posts").text();
    assert!(index.contains("Post created."), "{index}");
    assert!(index.contains("Hello"));
    let post = only_post(app);

    let show = app.get(&format!("/posts/{}", post.id)).text();
    assert!(show.contains("First post"));
    assert!(show.contains("No image yet."));
    let edit = app.get(&format!("/posts/{}/edit", post.id)).text();
    assert!(edit.contains("value=\"Hello\""), "{edit}");

    let res = app.post_form(
        &format!("/posts/{}", post.id),
        &[
            ("_method", "PUT"),
            ("title", "Hello again"),
            ("body", "Edited"),
        ],
    );
    assert_eq!(res.status(), 303);
    assert_eq!(only_post(app).title, "Hello again");
    assert!(
        app.get(&format!("/posts/{}", post.id))
            .text()
            .contains("Post updated.")
    );

    let res = app.post_form(&format!("/posts/{}", post.id), &[("_method", "DELETE")]);
    assert_eq!(res.status(), 303);
    assert_eq!(post_count(app), 0);
    assert_eq!(app.get(&format!("/posts/{}", post.id)).status(), 404);
}

#[test]
fn invalid_posts_are_sent_back_with_messages() {
    let d = demo();
    let app = &d.app;
    let res = app.post_form("/posts", &[("title", ""), ("body", "")]);
    assert_eq!(res.status(), 303);
    let form = app.get("/posts/create").text();
    assert!(form.contains("The title field is required."), "{form}");
    assert!(form.contains("The body field is required."), "{form}");

    let long = "x".repeat(256);
    app.post_form("/posts", &[("title", long.as_str()), ("body", "kept")]);
    let form = app.get("/posts/create").text();
    assert!(
        form.contains("The title field must not be greater than 255 characters."),
        "{form}"
    );
    assert!(
        form.contains(">kept</textarea>"),
        "the old input is kept: {form}"
    );
    assert_eq!(post_count(app), 0);
}

#[test]
fn an_image_is_uploaded_shown_replaced_and_removed() {
    let d = demo();
    let app = &d.app;
    let post = create_post(app);
    let mut spark = image_spark(app, &post);

    let token = upload_token(&spark, app, "cover.png", PNG.to_vec());
    spark.set("photo", &token).call("save", json!([])).send(app);
    let image = spark.data()["image"].as_str().unwrap().to_owned();
    assert!(
        image.starts_with("posts/") && image.ends_with(".png"),
        "{image}"
    );
    let file = d.root.join("storage/app/public").join(&image);
    assert_eq!(std::fs::read(&file).unwrap(), PNG);
    assert_eq!(only_post(app).image.as_deref(), Some(image.as_str()));
    let show = app.get(&format!("/posts/{}", post.id)).text();
    assert!(
        show.contains(&format!("src=\"/storage/{image}\"")),
        "{show}"
    );

    // A new image replaces the old file.
    let token = upload_token(&spark, app, "second.png", PNG.to_vec());
    spark.set("photo", &token).call("save", json!([])).send(app);
    let second = spark.data()["image"].as_str().unwrap().to_owned();
    assert_ne!(second, image);
    assert!(!file.exists(), "the replaced image is deleted");

    spark.call("remove", json!([])).send(app);
    assert!(spark.data()["image"].is_null());
    assert!(only_post(app).image.is_none());
    assert!(!d.root.join("storage/app/public").join(&second).exists());
}

#[test]
fn uploads_are_checked_for_size_and_type() {
    let d = demo();
    let app = &d.app;
    let post = create_post(app);
    let mut spark = image_spark(app, &post);

    // Nothing chosen.
    spark.call("save", json!([])).send(app);
    assert!(
        spark.html().contains("The photo field is required."),
        "{}",
        spark.html()
    );

    // Too large: the limit is 2048 kilobytes.
    let big = upload_token(&spark, app, "big.png", vec![0; 2048 * 1024 + 1]);
    spark.set("photo", &big).send(app);
    assert!(
        spark
            .html()
            .contains("The photo must not be greater than 2048 kilobytes."),
        "{}",
        spark.html()
    );

    // Another file type by name.
    let text = upload_token(&spark, app, "notes.txt", b"hello".to_vec());
    spark.set("photo", &text).send(app);
    assert!(
        spark
            .html()
            .contains("The photo must be a file of type: png, jpg, jpeg, gif, webp."),
        "{}",
        spark.html()
    );

    // An image name over bytes that are not an image.
    let fake = upload_token(&spark, app, "fake.png", b"<?php echo 1;".to_vec());
    spark.set("photo", &fake).call("save", json!([])).send(app);
    assert!(
        spark
            .html()
            .contains("The photo must be a PNG, JPEG, GIF or WebP image."),
        "{}",
        spark.html()
    );
    assert!(only_post(app).image.is_none());
    assert!(!d.root.join("storage/app/public/posts").exists());
}

#[test]
fn deleting_a_post_deletes_its_image() {
    let d = demo();
    let app = &d.app;
    let post = create_post(app);
    let mut spark = image_spark(app, &post);
    let token = upload_token(&spark, app, "cover.png", PNG.to_vec());
    spark.set("photo", &token).call("save", json!([])).send(app);
    let image = spark.data()["image"].as_str().unwrap().to_owned();
    let file = d.root.join("storage/app/public").join(image);
    assert!(file.exists());
    app.post_form(&format!("/posts/{}", post.id), &[("_method", "DELETE")]);
    assert!(!file.exists());
}

#[test]
fn image_bytes_are_sniffed() {
    use web_demo::app::sparks::post_image::is_image;
    assert!(is_image(PNG));
    assert!(is_image(&[0xFF, 0xD8, 0xFF, 0xE0]));
    assert!(is_image(b"GIF89a..."));
    assert!(is_image(b"RIFF\0\0\0\0WEBPVP8 "));
    assert!(!is_image(b"RIFF\0\0\0\0WAVE"));
    assert!(!is_image(b"hello"));
}
