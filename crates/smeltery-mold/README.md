# smeltery-mold

Mold is the template engine of [Smeltery](https://github.com/smelteryworks/smeltery). Templates are
`resources/views/<name>.mold.html` files with an `@`-directive syntax. In debug builds they are interpreted and
reloaded when a file changes; in release builds `#[derive(Mold)]` (from `smeltery-mold-macros`) compiles them to
Rust. Both modes produce the same bytes for the same data.

Apps use Mold through the `smeltery` crate (`smeltery::mold`, `#[derive(Mold)]`).

## Syntax

```text
{{ post.title }}              escaped output
{!! post.html !!}             raw output
{{-- a comment --}}           dropped
@{{ literal }}  @@            a literal "{{" and "@"

@if(user.admin) … @elseif(user.editor) … @else … @endif
@unless(posts) … @endunless
@for(post in posts) {{ loop.index }}/{{ loop.count }} … @empty No posts. @endfor
@for(key, value in settings) … @endfor
@for post in posts            (to the end of the line)

@extends("layouts/app")   @section("content") … @endsection   @section("title", post.title)
@yield("content")   @yield("title", "Default")
@include("partials/nav")   @include("partials/nav", { user: user, active: "home" })

<x-alert type="error" :count="errors | len">Body</x-alert>     renders components/alert
<x-forms.input name="email" />                                  renders components/forms/input
@slot("title") … @endslot                                       a named slot inside a component body

@csrf   @method("PUT")   @error("title") {{ message }} @enderror
@auth … @else … @endauth   @guest … @endguest
@spark("counter", { start: 5 })   @sparksScripts
```

- `loop` has `index` (from 1), `index0`, `first`, `last` and `count`.
- Inside a component only its props, `slot` (the body, already-safe HTML) and the named slots exist. Attribute
  names with `-` become `_` (`data-id` is `data_id`); `:attr` values are expressions, plain values are strings.
- A line holding only a block directive (`@if`, `@else`, `@endfor`, `@section`, …) is dropped with its line break.
- An `@word` that is not a Mold directive stays text, and an `@` right after an ASCII letter, digit or `_` never
  starts a directive, so CSS `@media` and e-mail addresses (`me@auth.example`) need no escaping. Write `x @endif`,
  not `x@endif`. Inside a block that takes it, a glued `x@else`, `x@elseif(…)` or `x@empty` followed by a space, a
  line break or `(` is an error (as text it would merge the block's branches); `x@@else` writes the text.

Expressions: `||`, `&&`, `== != < <= > >=`, `+ - * / %`, `!`, unary `-`, `a.b`, `a[i]`, literals (`1`, `1.5`,
`"text"`, `'text'`, `true`, `false`, `null`), parentheses, filters
`upper lower trim title len default(x) join(sep) json url` (`name | upper`, `tags | join(", ")`) and the functions `old("field")`, `session("status")`,
`route("posts.show", { post: post.id })` and `csrf_token()`.

`{{ }}` escapes `& < > " '` for text and quoted attribute values. `json` writes a value as a JavaScript literal with
`<`, `>`, `&`, `'`, U+2028 and U+2029 escaped: `{{ v | json }}` in attributes that run JavaScript (`x-data`,
`@click`, `onclick`), `{!! v | json !!}` inside `<script>`. `url` keeps relative, `http`, `https`, `mailto` and
`tel` URLs and writes `#` for any other (`javascript:`) and for an `&`, byte-order mark or control character before
the first `/`, `?` or `#`. `{!! !!}` never goes into an attribute (it writes `"` as it is).

Values: `false`, `0`, `0.0`, `""`, empty lists and maps, and `None` are falsy. Integer arithmetic stays integer
(overflow and division by zero are errors); a float operand makes a float; `+` joins two strings.

## Runtime engine

```rust,no_run
use smeltery_mold::{Engine, NoHost, to_value};

#[derive(serde::Serialize)]
struct Page {
    title: String,
}

let engine = Engine::new("resources/views");
let data = to_value(&Page { title: "Hello".into() })?;
let html = engine.render("pages/home", &data, &NoHost)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Errors name the file, line and column (`resources/views/pages/home.mold.html:3:7: unknown variable `titel``) and
carry a source excerpt; `Error::to_html` renders a development error page.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
