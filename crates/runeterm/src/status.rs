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
    /// On a charger, charging or not (a full battery stops charging but stays plugged in).
    pub plugged: bool,
}

pub fn read() -> Status {
    let now = chrono::Local::now();
    Status {
        time: now.format("%H:%M").to_string(),
        date: now.format("%a %-d %b").to_string(),
        battery: battery(std::path::Path::new("/sys/class/power_supply")),
    }
}

/// Every battery, averaged; charging if any is; plugged in if any charger (mains or USB) is
/// online, or a battery says it's on one.
pub fn battery(root: &std::path::Path) -> Option<Battery> {
    let mut levels = vec![];
    let (mut charging, mut plugged) = (false, false);
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let p = e.path();
        let read = |f: &str| std::fs::read_to_string(p.join(f)).map(|s| s.trim().to_string()).unwrap_or_default();
        match read("type").as_str() {
            "Battery" => {
                if let Ok(l) = read("capacity").parse::<u32>() {
                    levels.push(l.min(100));
                }
                match read("status").as_str() {
                    "Charging" => (charging, plugged) = (true, true),
                    "Full" | "Not charging" => plugged = true,
                    _ => {}
                }
            }
            "Mains" | "USB" | "USB_C" | "USB_PD" | "USB_PD_DRP" => plugged |= read("online") == "1",
            _ => {}
        }
    }
    if levels.is_empty() {
        return None;
    }
    Some(Battery { level: (levels.iter().sum::<u32>() / levels.len() as u32) as u8, charging, plugged })
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
        mk("AC", &[("type", "Mains\n"), ("online", "0\n")]);
        mk("BAT0", &[("type", "Battery\n"), ("capacity", "80\n"), ("status", "Discharging\n")]);
        mk("BAT1", &[("type", "Battery\n"), ("capacity", "60\n"), ("status", "Charging\n")]);
        assert_eq!(battery(&root), Some(Battery { level: 70, charging: true, plugged: true }));
        // Full on the charger: not charging, but plugged in.
        mk("BAT1", &[("status", "Full\n"), ("capacity", "100\n")]);
        mk("BAT0", &[("status", "Full\n"), ("capacity", "100\n")]);
        assert_eq!(battery(&root), Some(Battery { level: 100, charging: false, plugged: true }));
        // Unplugged: the charger is offline and nothing says otherwise.
        mk("BAT1", &[("status", "Discharging\n")]);
        mk("BAT0", &[("status", "Discharging\n")]);
        assert_eq!(battery(&root).map(|b| b.plugged), Some(false));
        // A charger online but a battery reporting "Discharging" (some firmwares): plugged.
        mk("AC", &[("online", "1\n")]);
        assert_eq!(battery(&root).map(|b| b.plugged), Some(true));
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(battery(&root), None);
    }
}
