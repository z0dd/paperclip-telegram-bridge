//! Domain types. Depend neither on Paperclip JSON nor on Telegram.

pub mod interaction;
pub mod issue;
pub mod run;

pub use interaction::*;
pub use issue::*;
pub use run::*;
