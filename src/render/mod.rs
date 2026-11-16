//! domain → Telegram: HTML templates, escaping, splitting, keyboards, texts (ru).

pub mod callback;
pub mod text;

pub use callback::{Callback, Op};
pub use text::{escape, split, truncate_chars, truncate_utf16, utf16_len, visible_len};

use crate::telegram::Button;

pub fn button(caption: &str, cb: Callback) -> Button {
    Button {
        text: text::button_text(caption),
        callback_data: cb.encode(),
    }
}
