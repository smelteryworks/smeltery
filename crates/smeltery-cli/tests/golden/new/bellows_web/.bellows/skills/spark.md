# Skill: write a Spark (live component)

A Spark is a Rust struct (state) with a Mold view and actions; the page updates without reloading and without
writing JavaScript.

1. Generate it: `smeltery make:spark TodoList`. It creates `app/sparks/todo_list.rs` and
   `resources/views/sparks/todo_list.mold.html` and registers it in `app/sparks/mod.rs`.
2. State: the struct's fields. Only `#[spark(model)]` fields accept values from the page (`wire:model="field"`);
   plain model fields take no objects; a struct field the page edits is `#[spark(model(fields = "title, body"))]`
   (`wire:model="form.title"`). Never put secrets in fields: the state travels signed but readable in the page.
3. Actions: `pub async fn` methods taking `&mut self` in the `#[actions] impl` block, optionally
   `ctx: &mut SparkCtx` and parameters. Use them in the view with `wire:click="add"`, `wire:click="remove(3)"`,
   `wire:submit="save"`; a value from data goes through `json`: `wire:click="remove({{ item.id | json }})"`.
   Restrict with `#[guard(auth)]`, and re-check permissions inside every action with `ctx.auth()` and the database
   (a snapshot can be sent again within its session). `mount(&mut self, ctx)` runs on the first render
   (`ctx.prop("name")` reads props).
4. Show it on a page: `@spark("todo_list")` or `@spark("todo_list", { start: 1 })`. The layout already loads the
   runtime with `@sparksScripts`.
5. Test it with `smeltery::sparks::testing::TestSpark::from_html(&app.get("/").text(), "todo_list")`, then
   `.set("field", value)`, `.call("save", smeltery::json!([])).send(&app)`, and check `.data()`.
6. Run `smeltery test`.
