//! `#[derive(Mold)]` through the facade: templates returned from handlers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use smeltery::prelude::*;
use smeltery::testing::TestApp;

#[derive(serde::Serialize)]
struct Post {
    id: u32,
    title: String,
}

#[derive(Mold)]
#[mold("posts", dir = "tests/app/resources/views")]
struct PostsIndex {
    heading: String,
    posts: Vec<Post>,
}

async fn index() -> PostsIndex {
    PostsIndex {
        heading: "Posts".into(),
        posts: vec![
            Post {
                id: 1,
                title: "<First>".into(),
            },
            Post {
                id: 2,
                title: "Second".into(),
            },
        ],
    }
}

async fn fallible(Path(n): Path<u32>) -> Result<PostsIndex> {
    if n == 0 {
        return Err(Error::not_found());
    }
    Ok(PostsIndex {
        heading: format!("Page {n}"),
        posts: vec![],
    })
}

async fn show() {}

fn app() -> TestApp {
    TestApp::new(|mut b: AppBuilder| {
        b.settings_mut().root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app");
        b.routes(|r| {
            r.get("/posts", index);
            r.get("/pages/{n}", fallible);
            r.get("/posts/{post}", show).name("posts.show");
        })
    })
}

const EXPECTED: &str = "<!doctype html>\n<title>Posts</title>\n<h1>Posts</h1>\n\
<div class=\"card\"><a href=\"/posts/1\">&lt;First&gt;</a></div>\n\n\
<div class=\"card\"><a href=\"/posts/2\">Second</a></div>\n\n\n";

#[test]
fn a_handler_returns_a_template() {
    let app = app();
    let res = app.get("/posts");
    assert_eq!(res.status(), 200);
    assert_eq!(res.header("content-type"), Some("text/html; charset=utf-8"));
    assert_eq!(res.text(), EXPECTED);
}

#[test]
fn both_render_paths_agree() {
    let app = app();
    let t = smeltery::testing::TestApp::block_on(&app, index());
    let host = smeltery::view::RequestHost::new(app.app().clone(), Default::default());
    assert_eq!(t.render_compiled(&host).unwrap(), EXPECTED);
    assert_eq!(
        t.render_runtime_with(app.app().views(), &host).unwrap(),
        EXPECTED
    );
}

#[test]
fn result_of_a_template_works() {
    let app = app();
    let res = app.get("/pages/3");
    assert_eq!(res.status(), 200);
    assert!(
        res.text().contains("<h1>Page 3</h1>\n<p>No posts.</p>"),
        "{}",
        res.text()
    );
    assert_eq!(app.get("/pages/0").status(), 404);
}
