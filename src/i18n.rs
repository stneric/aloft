use std::sync::OnceLock;

fn german() -> bool {
    static DE: OnceLock<bool> = OnceLock::new();
    *DE.get_or_init(|| {
        for var in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(v) = std::env::var(var) {
                if !v.is_empty() {
                    return v.starts_with("de");
                }
            }
        }
        false
    })
}

pub fn t(en: &'static str, de: &'static str) -> &'static str {
    if german() {
        de
    } else {
        en
    }
}
