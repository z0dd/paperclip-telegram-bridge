//! `/status`, `/backlog`, `/help` (architecture §8.4).

use super::dialog::Effects;
use super::{Ctx, Result};
use crate::paperclip::PaperclipApi;
use crate::telegram::{BotCommand, TelegramApi};

pub const HELP: &str = "Я пересылаю вопросы и запросы подтверждения от агентов Paperclip и отправляю ваши ответы обратно.

Как отвечать:
• нажмите кнопку под вопросом;
• для текстового ответа нажмите «✍️ Ответить текстом» или ответьте (reply) на сообщение;
• формы из нескольких вопросов заполняются по шагам, ответ уходит после «Отправить»;
• запросы на действия инструментов (🛠) и доступ к секретам (🔑) можно разрешить или отклонить; необратимые действия требуют второго подтверждения.

Команды:
/status — что ждёт ответа и что сейчас в работе
/backlog — ближайшие задачи
/cancel — отменить ввод текста
/help — эта справка

Если вопрос уже решён в Paperclip, сообщение обновится само.";

pub fn bot_commands() -> Vec<BotCommand> {
    [
        ("status", "Что ждёт ответа и что в работе"),
        ("backlog", "Ближайшие задачи"),
        ("cancel", "Отменить ввод текста"),
        ("help", "Справка"),
    ]
    .into_iter()
    .map(|(c, d)| BotCommand {
        command: c.into(),
        description: d.into(),
    })
    .collect()
}

pub async fn handle<P: PaperclipApi, T: TelegramApi>(
    _ctx: &Ctx<P, T>,
    name: &str,
) -> Result<Effects> {
    Ok(match name {
        "start" | "help" => Effects::message(crate::render::escape(HELP)),
        _ => Effects::message("Неизвестная команда. /help"),
    })
}
