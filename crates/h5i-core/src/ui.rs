// src/ui.rs (イメージ)
use console::{style, Emoji};

pub static LOOKING: Emoji<'_, '_> = Emoji("◈ ", "");
pub static SUCCESS: Emoji<'_, '_> = Emoji("✔ ", "");
pub static ERROR: Emoji<'_, '_> = Emoji("✖ ", "");
pub static WARN: Emoji<'_, '_> = Emoji("⚠ ", "");
pub static STEP: Emoji<'_, '_> = Emoji("➜ ", "");

pub struct UI;

impl UI {
    pub fn action(msg: &str) {
        println!("{} {}", style("➜").cyan().bold(), style(msg).bold());
    }

    pub fn success(msg: &str) {
        println!("{} {}", style("✔").green(), msg);
    }

    pub fn info(msg: &str) {
        println!("{} {}", style("ℹ").blue(), style(msg).dim());
    }

    pub fn warning(msg: &str) {
        println!("{} {}", style("⚠").yellow(), style(msg).yellow());
    }

    pub fn error(msg: &str) {
        eprintln!("{} {}", style("✖").red().bold(), style(msg).red());
    }
}
