# Skill: protect a route with authentication

The app has login, registration and password reset (`app/controllers/auth/`), and the middleware aliases `auth`
(guests are sent to `/login`) and `guest` (logged-in users are sent to `/dashboard`).

1. In `routes/web.rs`, add `.middleware("auth")` to the route:

   ```rust
   r.get("/settings", crate::app::controllers::settings::index).name("settings").middleware("auth");
   ```

   Several routes at once: `r.group("/admin", |r| { … }).middleware("auth");`.
2. In the handler, take `auth: smeltery::auth::Auth` and load the user with
   `auth.user::<crate::app::models::User>().await?` (or `auth.id()` for the id only).
3. In views, `@auth … @endauth` / `@guest … @endguest` show parts by login state.
4. Test it: `TestApp::new(build).get("/settings")` answers 303 with `location: /login` for a guest; log in first with
   `post_form("/login", &[("email", …), ("password", …)])` (the test cookie jar keeps the session), or use
   `acting_as(user_id)`.
5. Run `smeltery test`.
