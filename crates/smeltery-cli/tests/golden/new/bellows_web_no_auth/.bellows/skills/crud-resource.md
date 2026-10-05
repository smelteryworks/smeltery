# Skill: add a CRUD resource

Goal: a model with its table, a resource controller with views and routes, a factory and a seeder.

1. Generate everything in one command (fields are `name:type`, `?` makes a field nullable; types: `string`, `text`,
   `integer`, `bigint`, `bool`, `float`, `date`, `datetime`, `json`, `uuid`, `<name>_id:foreign`, `file` for an upload):

   ```sh
   smeltery make:model Post title:string body:text? published:bool -mcrfs
   ```

   It creates `app/models/post.rs`, `database/migrations/m<timestamp>_create_posts_table.rs`,
   `app/controllers/posts.rs`, `resources/views/posts/{index,create,show,edit}.mold.html`,
   `database/factories/post_factory.rs` and `database/seeders/post_seeder.rs`, and registers them (the
   `r.resource("/posts")…` entry in `routes/web.rs`, the migration, the seeder). All seven routes are public:
   anyone can create, edit and delete records.
2. Run the migration: `smeltery migrate`. Seed if wanted: `smeltery db:seed`.
3. Adjust the validation rules on `PostForm` in `app/controllers/posts.rs` (`#[validate(required, max = 255)]`, …). A `file` field
   has a `mimes` rule (images for a name such as `image` or `photo`, else images, PDF and office or text documents);
   add types as needed, never `html`, `svg`, `xml` or `js`, which a browser runs as a page of the app.
4. Add a test in `tests/` with `TestApp`: create a record through `post_form("/posts", …)` and check `/posts`.
5. Run `smeltery test`.
