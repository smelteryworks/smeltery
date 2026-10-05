//! The sentences Temper's default answers use (flashed as `status` / `error`, or put on a form field). An app that
//! wants other words overrides the answer ([`TemperResponses`](crate::TemperResponses)) or the view.

/// A login with a wrong address or password (on the `email` field).
pub const FAILED_LOGIN: &str = "These credentials do not match our records.";

/// A reset link was asked for (the same for every address, with or without an account).
pub const RESET_LINK_SENT: &str =
    "If that e-mail address has an account, a password reset link has been sent to it.";

/// A password was reset.
pub const PASSWORD_RESET: &str = "Your password has been reset. Log in with the new password.";

/// A reset link that is wrong, used or expired.
pub const RESET_FAILED: &str = "This password reset link is invalid or has expired.";

/// An e-mail address was verified.
pub const EMAIL_VERIFIED: &str = "Your e-mail address is verified.";

/// A verification link was sent again.
pub const VERIFICATION_LINK_SENT: &str =
    "A new verification link has been sent to your e-mail address.";

/// A wrong password on the password confirmation form (on the `password` field).
pub const WRONG_PASSWORD: &str = "The provided password was incorrect.";

/// A wrong current password on the password update form (on the `current_password` field).
pub const WRONG_CURRENT_PASSWORD: &str =
    "The provided password does not match your current password.";

/// The profile was updated.
pub const PROFILE_UPDATED: &str = "Your profile has been updated.";

/// The password was updated.
pub const PASSWORD_UPDATED: &str = "Your password has been updated.";

/// A wrong two-factor or recovery code (on `code` / `recovery_code`).
pub const TWO_FACTOR_FAILED: &str = "The provided two factor authentication code was invalid.";

/// The pending two-factor login is gone (expired, too many wrong codes, or the password changed).
pub const TWO_FACTOR_EXPIRED: &str = "Your login has expired. Please log in again.";

/// Too many two-factor codes for the account (`{}`: seconds).
pub const TWO_FACTOR_TOO_MANY: &str = "Too many attempts. Please try again in {} seconds.";

/// Two-factor authentication was enabled (a first code still confirms it when confirmation is on).
pub const TWO_FACTOR_ENABLED: &str = "Two-factor authentication is enabled.";

/// Two-factor authentication was confirmed with a first code.
pub const TWO_FACTOR_CONFIRMED: &str = "Two-factor authentication is confirmed.";

/// Two-factor authentication was turned off.
pub const TWO_FACTOR_DISABLED: &str = "Two-factor authentication is turned off.";

/// Enabling while a confirmed enrolment exists.
pub const TWO_FACTOR_ALREADY_ENABLED: &str =
    "Two-factor authentication is already enabled. Turn it off first.";

/// New recovery codes were generated.
pub const RECOVERY_CODES_GENERATED: &str = "New recovery codes have been generated.";
