//! Telegram notification: a single digest message per run.

use std::env;
use std::process::Command;

/// One package's outcome (UPDATED / UP_TO_DATE / BROKEN / FAILED / WARN).
pub struct PkgResult {
    name: String,
    status: String,
    detail: String,
}

impl PkgResult {
    pub fn new(name: &str, status: &str, detail: &str) -> Self {
        Self {
            name: name.to_string(),
            status: status.to_string(),
            detail: detail.to_string(),
        }
    }

    pub fn render(&self) -> String {
        let icon = match self.status.as_str() {
            "UPDATED" => "✅",
            "UP_TO_DATE" => "⏸",
            "BROKEN" => "⚠️",
            "FAILED" => "❌",
            "WARN" => "🟡",
            _ => "•",
        };
        let status = self.status.to_lowercase().replace('_', " ");
        let mut line = format!("{icon} {}: {status}", self.name);
        if !self.detail.is_empty() {
            line.push_str(&format!(" ({})", self.detail));
        }
        line
    }
}

/// Sends a digest to a single chat via the Bot API. No-op if disabled.
pub struct TelegramNotifier {
    token: String,
    chat_id: String,
}

impl TelegramNotifier {
    pub fn from_env() -> Self {
        Self {
            token: env::var("TG_BOT_TOKEN").unwrap_or_default(),
            chat_id: env::var("TG_CHAT_ID").unwrap_or_default(),
        }
    }

    fn enabled(&self) -> bool {
        !self.token.is_empty() && !self.chat_id.is_empty()
    }

    fn send(&self, text: &str) {
        if !self.enabled() {
            return;
        }
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.token);
        let result = Command::new("curl")
            .args(["-sS", "--max-time", "30"])
            .arg("--data-urlencode")
            .arg(format!("chat_id={}", self.chat_id))
            .arg("--data-urlencode")
            .arg(format!("text={text}"))
            .arg("--data-urlencode")
            .arg("disable_web_page_preview=true")
            .arg(&url)
            .output();
        // Network failures must not abort the run.
        match result {
            Ok(out) if out.status.success() => {
                let body = String::from_utf8_lossy(&out.stdout);
                if !body.contains("\"ok\":true") {
                    println!("[telegram] API error: {}", body.trim());
                }
            }
            Ok(out) => {
                println!(
                    "[telegram] send failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Err(e) => println!("[telegram] send failed: {e}"),
        }
    }

    pub fn send_digest(&self, results: &[PkgResult], run_id: &str) {
        if results.is_empty() {
            return;
        }
        let header = format!("🖥 AUR updater — {run_id}");
        let body = results
            .iter()
            .map(PkgResult::render)
            .collect::<Vec<_>>()
            .join("\n");
        self.send(&format!("{header}\n{body}"));
    }
}

/// Local "YYYY-MM-DD HH:MM" timestamp (via `date`, as `datetime.now()` did).
pub fn run_id() -> String {
    Command::new("date")
        .arg("+%Y-%m-%d %H:%M")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}
