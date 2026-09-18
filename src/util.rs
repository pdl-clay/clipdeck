//! Formatação curta para humanos (CLI e UI).

use std::time::{SystemTime, UNIX_EPOCH};

pub fn human_age(created_at: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let seconds = ((now - created_at) / 1000).max(0);
    match seconds {
        0..=59 => "agora".to_string(),
        60..=3599 => format!("há {} min", seconds / 60),
        3600..=86399 => format!("há {} h", seconds / 3600),
        _ => format!("há {} d", seconds / 86400),
    }
}

pub fn human_bytes(bytes: i64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idade_legivel() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert_eq!(human_age(now), "agora");
        assert_eq!(human_age(now - 5 * 60_000), "há 5 min");
        assert_eq!(human_age(now - 3 * 3_600_000), "há 3 h");
        assert_eq!(human_age(now - 2 * 86_400_000), "há 2 d");
    }

    #[test]
    fn bytes_legiveis() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
    }
}
