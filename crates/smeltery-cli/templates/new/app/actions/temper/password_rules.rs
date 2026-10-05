//! The password rules, in one place: the registration, reset and password change forms declare their fields
//! through `password_form!`, so a stricter rule here applies to all three.

/// Declares a form with the new-password fields after its own: `password` (required, at least 8 characters, typed
/// twice) and `password_confirmation`.
macro_rules! password_form {
    ($(#[$meta:meta])* pub struct $name:ident { $($fields:tt)* }) => {
        $(#[$meta])*
        pub struct $name {
            $($fields)*
            /// The new password.
            #[validate(required, min = 8, confirmed)]
            pub password: String,
            /// The new password again (the `confirmed` rule compares them).
            pub password_confirmation: Option<String>,
        }
    };
}

pub(crate) use password_form;
