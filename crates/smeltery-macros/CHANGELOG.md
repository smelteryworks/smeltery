# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added
- `#[derive(Validate)]` with the rules `required`, `email`, `url`, `min`, `max`, `between`, `numeric`, `integer`,
  `alpha`, `alpha_num`, `alpha_dash`, `in_list`, `confirmed`, `same`, `accepted`, `unique`, `exists`, `mimes`
  (`mimes = "png,jpg"`, for uploaded files) and `message = "…"`.
- `#[derive(Spark)]` (`name`, `view`, `stream`, `dir`, `crate`; field attributes `model`,
  `#[spark(model(fields = "title, body"))]` listing the keys the page may set (`form.title`), and
  `upload(max = …, mimes = "…")`) and the `#[actions]` attribute (`pub async fn` actions with typed parameters,
  `#[guard(auth)]` / `#[guard(guest)]`, the `mount`, `updated` and `rendering` hooks, and the
  `can_stream(&mut self, ctx) -> Result<bool>` hook that implements `Actions::stream_hook`).
- `#[actions]`: `#[on("anvil:<channel>", "<event>")]` on an `async fn (&mut self, ctx: &mut SparkCtx, data: T)`
  makes it a Sparks listener (checked at compile time: the `anvil:` source, channel characters, `{field}`, the event
  name, at most 16 per component); listeners are not actions.
- `#[derive(BroadcastEvent)]` with `#[broadcast(public = "…", private = "orders.{order_id}", presence = "…", as = "…")]`:
  implements `smeltery::anvil::BroadcastEvent` (channels from fields, an unknown field is a compile error).
- `#[derive(Alloy)]` with `#[alloy("component/name")]`: implements `smeltery::alloy::Component` and makes the
  struct a response (an Alloy page).

### Security
- `#[derive(Spark)]` refuses `#[serde(rename …)]`, `rename_all`, `rename_all_fields`, `alias` and `flatten` on the
  struct and its fields (a compile error): the model allow-list names fields by their Rust names, so the state's keys
  must be those names.

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
