//! The Hallmark migration template renders to the migration the crate documents.
#![allow(clippy::unwrap_used)]

const TEMPLATE: &str =
    include_str!("../templates/hallmark/create_personal_access_tokens_table.rs.jinja");

#[test]
fn the_hallmark_migration_template_renders() {
    let mut env = minijinja::Environment::new();
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    env.set_keep_trailing_newline(true);
    let out = env
        .render_str(
            TEMPLATE,
            minijinja::context! {
                hallmark_migration_name => "2026_10_05_120000_create_personal_access_tokens_table"
            },
        )
        .unwrap();
    assert!(
        out.contains("\"2026_10_05_120000_create_personal_access_tokens_table\""),
        "{out}"
    );
    assert!(out.contains("smeltery::hallmark::migrations::up(schema).await"));
    assert!(out.contains("smeltery::hallmark::migrations::down(schema).await"));
    assert!(out.contains("pub struct CreatePersonalAccessTokensTable;"));
    assert!(!out.contains("{{") && out.ends_with("}\n"));
    // Undefined variables are an error, so the template names exactly one.
    assert!(env.render_str(TEMPLATE, minijinja::context! {}).is_err());
}
