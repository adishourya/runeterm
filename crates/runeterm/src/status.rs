//! The sidebar's quiet status: the time, and the battery (from `/sys/class/power_supply`).

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// "14:05"
    pub time: String,
    /// "Sun 9 Nov"
    pub date: String,
    pub battery: Option<Battery>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Battery {
    /// 0–100.
    pub level: u8,
    pub charging: bool,
}

pub fn read() -> Status {
    let now = chrono::Local::now();
    Status {
        time: now.format("%H:%M").to_string(),
        date: now.format("%a %-d %b").to_string(),
        battery: battery(std::path::Path::new("/sys/class/power_supply")),
    }
}

/// Every battery, averaged; charging if any is.
pub fn battery(root: &std::path::Path) -> Option<Battery> {
    let mut levels = vec![];
    let mut charging = false;
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let p = e.path();
        let read = |f: &str| std::fs::read_to_string(p.join(f)).map(|s| s.trim().to_string()).unwrap_or_default();
        if read("type") != "Battery" {
            continue;
        }
        if let Ok(l) = read("capacity").parse::<u32>() {
            levels.push(l.min(100));
        }
        charging |= matches!(read("status").as_str(), "Charging" | "Full");
    }
    if levels.is_empty() {
        return None;
    }
    Some(Battery { level: (levels.iter().sum::<u32>() / levels.len() as u32) as u8, charging })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_batteries_and_skips_other_supplies() {
        let root = std::env::temp_dir().join(format!("runeterm-power-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mk = |name: &str, files: &[(&str, &str)]| {
            let d = root.join(name);
            std::fs::create_dir_all(&d).unwrap();
            for (f, v) in files {
                std::fs::write(d.join(f), v).unwrap();
            }
        };
        mk("AC", &[("type", "Mains\n"), ("online", "1\n")]);
        mk("BAT0", &[("type", "Battery\n"), ("capacity", "80\n"), ("status", "Discharging\n")]);
        mk("BAT1", &[("type", "Battery\n"), ("capacity", "60\n"), ("status", "Charging\n")]);
        assert_eq!(battery(&root), Some(Battery { level: 70, charging: true }));
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(battery(&root), None);
    }
}
