# Skill: add a Vue page (Alloy)

A page is a Vue component in `resources/js/pages/`; a Rust controller decides which page to show and with
which props. The first visit gets HTML (`resources/views/app.mold.html`), every later visit gets the page as JSON
(the Inertia protocol, spoken by Alloy).

1. Generate it: `smeltery make:page Pricing`. It creates `app/controllers/pricing.rs` (returning
   `alloy::render("Pricing")`), `resources/js/pages/Pricing.vue` and the route
   `r.get("/pricing", …).name("pricing")` in `routes/web.rs`. `smeltery make:model Post title:string --all` writes
   a whole resource: a controller and the pages `posts/{Index,Create,Show,Edit}.vue`.
2. Props: chain them on the page in the controller:

   ```rust
   use smeltery::alloy::{self, Page};

   pub async fn show(db: Db) -> smeltery::Result<Page> {
       let plans = Plan::all(&db).await?;
       Ok(alloy::render("Pricing")
           .with("plans", plans) // sent on every visit
           .optional("stats", move || async move { Ok(Stats::load().await?) }) // only when a reload asks
           .defer("reviews", move || async move { Ok(Review::recent().await?) })) // loaded right after the page
   }
   ```

   Or one struct per page: `#[derive(serde::Serialize, smeltery::Alloy)] #[alloy("Pricing")] pub struct
   Pricing { pub plans: Vec<Plan> }`, returned from the handler.
3. Every prop is readable in the browser. Never pass a model with secrets (password hashes, tokens): send a struct
   with only the fields the page needs. Values flashed with `session.flash(key, value)` reach the next page as
   `usePage().flash` and are public too.
4. In the page, the props are the component's props (`defineProps<{ plans: Plan[] }>()`).
   Shared props (`app`, from `app/providers/alloy.rs`) are in `usePage().props`.
   Partial reloads: `router.reload({ only: ['stats'] })`; deferred props: `<Deferred data="reviews">…</Deferred>`.
   Links: `<Link href="/pricing">`; forms: `useForm({ … })` and `form.post('/url')`, with the messages of a failed
   validation in `form.errors.field`.
5. Test it in `tests/`: `app.get_alloy("/pricing").assert_component("Pricing").assert_prop("plans.0.name", "Basic")`
   (`use smeltery::alloy::testing::{AlloyAssertions as _, AlloyRequests as _};`) and
   `assert_page_file_exists("Pricing")`.
6. Run `smeltery test` and `npm run types`.
