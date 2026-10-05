# smeltery-macros

Derive macros for [Smeltery](https://github.com/smelteryworks/smeltery): `#[derive(Validate)]` implements
`smeltery::validation::Validate` for a form struct from `#[validate(...)]` field attributes.

Apps use it through the `smeltery` crate:

```rust
use smeltery::prelude::*;
# use serde::Deserialize;

#[derive(Deserialize, Validate)]
pub struct RegisterForm {
    #[validate(required, max = 255)]
    pub name: String,
    #[validate(required, email, unique(table = "users", column = "email"))]
    pub email: String,
    #[validate(required, min = 8, confirmed)]
    pub password: String,
    pub password_confirmation: String,
    #[validate(integer, between(1, 120))]
    pub age: Option<i64>,
}
# fn main() {}
```

The rules, their messages and the `Valid<T>` extractor are described in the
[Smeltery README](https://github.com/smelteryworks/smeltery#validation).

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
