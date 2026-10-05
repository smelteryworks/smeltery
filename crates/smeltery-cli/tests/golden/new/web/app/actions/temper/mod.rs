//! What Temper (`app/providers/temper.rs`) calls for the authentication forms: each action has its form (the
//! validation rules) and the work only this app knows (creating a user, writing the profile).

pub mod create_new_user;
pub mod password_rules;
pub mod reset_user_password;
pub mod update_user_password;
pub mod update_user_profile_information;

pub use create_new_user::CreateNewUser;
pub use reset_user_password::ResetUserPassword;
pub use update_user_password::UpdateUserPassword;
pub use update_user_profile_information::UpdateUserProfileInformation;
