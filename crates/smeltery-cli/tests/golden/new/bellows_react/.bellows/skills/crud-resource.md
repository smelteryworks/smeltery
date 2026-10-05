# Skill: add a CRUD resource

Goal: a model with its table, a resource controller with React pages and routes, a factory and a seeder.

1. Generate everything in one command (fields are `name:type`, `?` makes a field nullable; types: `string`, `text`,
   `integer`, `bigint`, `bool`, `float`, `date`, `datetime`, `json`, `uuid`, `<name>_id:foreign`, `file` for an upload):

   ```sh
   smeltery make:model Post title:string body:text? published:bool -mcrfs
   ```

   It creates `app/models/post.rs`, `database/migrations/m<timestamp>_create_posts_table.rs`,
   `app/controllers/posts.rs` (returning `alloy::render("posts/index")` and the other pages),
   `resources/js/pages/posts/{index,create,show,edit}.tsx` (the forms use `useForm`),
   `resources/js/types/post.ts` (the record's TypeScript type),
   `database/factories/post_factory.rs` and `database/seeders/post_seeder.rs`, and registers them (the
   `r.resource("/posts")…` entries in `routes/web.rs`, the migration, the seeder). `index` and `show` are public; `create`,
   `store`, `edit`, `update` and `destroy` are a second `r.resource("/posts")` entry with `.middleware("auth")`.
2. Run the migration: `smeltery migrate`. Seed if wanted: `smeltery db:seed`.
3. Adjust the validation rules on `PostForm` in `app/controllers/posts.rs` (`#[validate(required, max = 255)]`, …). The
   pages show a failed rule's message under its field (`form.errors.title`). A `file` field
   has a `mimes` rule (images for a name such as `image` or `photo`, else images, PDF and office or text documents);
   add types as needed, never `html`, `svg`, `xml` or `js`, which a browser runs as a page of the app.
4. Every signed-in user may change every record. To make the whole resource private, add `.middleware("auth")` to
   the first entry too; where records belong to a user, check `auth.id()` against the record in `edit`, `update` and
   `destroy`.
5. The list and record pages get every field of the model as props (readable in the browser): when a
   model holds something private, send a struct with the public fields instead.
6. Add a test in `tests/` with `TestApp`: create a record through `post_alloy("/posts", &json)` and check
   `get_alloy("/posts").assert_prop("posts.0.title", …)`.
7. Run `smeltery test` and `npm run types`.
