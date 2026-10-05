# Skill: make a model searchable

The app has full-text search (Prospect, `.prospect(app::providers::search::register)` in `bootstrap/app.rs`). The
`database` driver (`PROSPECT_DRIVER=database`) uses the database's own full-text index (SQLite FTS5, PostgreSQL
`tsvector`, MySQL `FULLTEXT`), which the database keeps current itself.

1. A new model: `smeltery make:model Post title:string body:text user_id:foreign --searchable --all`. It writes
   `impl Searchable` in `app/models/post.rs` (every `string` / `text` field searched, the first with `Weight::A`;
   `foreign` fields as filters), the index in the create-table migration (`SearchIndex::on("posts")…create`),
   `p.model::<crate::app::models::Post>();` in `app/providers/search.rs`, and a list page that searches (`?q=`,
   paginated, the label highlighted, `throttle:60,1`). An existing model: add `--searchable` to `make:model` with the
   same fields before the table exists, or write `impl Searchable` by hand and a migration with
   `SearchIndex::on("posts").text("title", Weight::A).create(schema)`; the columns and weights must match.
2. Search in a handler (`prospect: smeltery::prospect::Prospect`, `use smeltery::prospect::Searchable as _`):
   `Post::search(&prospect, &q).highlight(["title"]).paginate(page).await?` gives a `Page<Hit<Post>>`; `where_eq`,
   `where_in` and `where_between` take declared `i.filter(…)` columns only.
3. Records that are private: declare `i.scoped_by("team_id")` (then every search needs `.within(team)`) or
   `i.only_when("published")`, or add `.where_eq(…)` to the search; never show a hit the user may not see.
4. Highlights are text segments (`{"text", "matched"}`), never HTML: render each one escaped and wrap the matched
   ones in `<mark>`.
5. Change the searched columns or weights with a new migration that calls `SearchIndex::on(…).rebuild(schema)`.
6. Test it: searches work in `TestApp` on in-memory SQLite; create rows with the model's factory, then
   `app.get("/posts?q=word")`. `smeltery prospect:status` shows each model's index.
7. Run `smeltery test`.
