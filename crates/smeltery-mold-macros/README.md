# smeltery-mold-macros

`#[derive(Mold)]` for [Smeltery](https://github.com/smelteryworks/smeltery): compiles a Mold template
(`resources/views/<name>.mold.html`) to Rust at build time and implements `Template` for the struct.

Apps use it through the `smeltery` crate:

```rust
use smeltery::mold::Template;
# #[derive(serde::Serialize)]
# struct Post {
#     title: String,
# }

#[derive(smeltery::Mold)]
#[mold("posts/index")]
struct PostsIndex {
    title: String,
    posts: Vec<Post>,
}
# fn main() {}
```

- `#[mold("name")]` names the template; `crate = "path"` sets the path of the Mold crate (default
  `::smeltery::mold`) and `dir = "path"` the views directory relative to `CARGO_MANIFEST_DIR` (default
  `resources/views`).
- Template variables are the struct's fields; a variable without a field, a template syntax error and a missing
  template are compile errors naming the `.mold.html` file, line and column.
- `render_compiled` runs the generated code; `render_runtime` renders the same template through the runtime engine
  (each field must implement `serde::Serialize`). `render` picks runtime in debug builds and compiled in release
  builds, and both produce the same bytes.
- With the default crate path the struct also implements `smeltery::http::IntoResponse`: a handler returns it
  and the view middleware renders it with the request's data.
- Every template file the derive read is included in the build, so editing one rebuilds the crate.
- The struct must have named fields; generic parameters and where-clauses are kept.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
