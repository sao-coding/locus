//! `%APPDATA%\Locus\settings.json` and the language choice.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const LANGUAGES: &[&str] = &["en", "zh-TW", "zh-CN"];

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// "light", "dark" or "system" (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Row limit of new tabs; 0 = unlimited; missing = 100,000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_limit: Option<u64>,
    /// UI and editor font size in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    /// Rows a result may keep in memory; 0 = no cap; missing = 1,000,000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<u64>,
    /// Minutes a paused (paged) query keeps its server cursor; missing = 10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_idle_minutes: Option<u64>,
    /// Turn transitions off.
    #[serde(default)]
    pub reduce_motion: bool,
    /// Do not count the rows of a paused query in the background.
    #[serde(default)]
    pub no_count: bool,
    /// Do not produce the query plan automatically after a SELECT.
    #[serde(default)]
    pub no_auto_plan: bool,
    /// Window material: "opaque" (default), "mica", "blur", "mica_alt", "opaque"; read it with `material()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    /// How much more see-through than the default the sidebar and title bar are (0, 0.1, 0.2, 0.3); old files hold 0.15..0.75, see `migrate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translucency: Option<f32>,
    /// Strength of the Space colour in the window background (0..2, default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_tint: Option<f32>,
    /// Ask before a write on a non-Production connection (default on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_nonprod_writes: Option<bool>,
    /// Format SQL: upper-case keywords (default on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_uppercase: Option<bool>,
    /// Format SQL: spaces per indent level, 2 or 4 (default 4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_indent: Option<u8>,
    /// Production connections open query tabs in Manual transaction mode (default on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prod_manual_tx: Option<bool>,
    /// Minutes an open Manual transaction may sit idle before a warning; 0 = never; missing = 10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_idle_minutes: Option<u64>,
    /// Alternate the tone of odd and even result rows (default on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid_stripes: Option<bool>,
    /// Look for a new release on GitHub after start (default on); see `update`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_check: Option<bool>,
}

impl Settings {
    /// Whether the automatic update check is on (missing = on).
    pub fn update_check(&self) -> bool {
        self.update_check.unwrap_or(true)
    }
}

pub fn app_dir() -> Option<PathBuf> {
    // LOCUS_DATA_DIR relocates everything (UI tests).
    if let Some(d) = std::env::var_os("LOCUS_DATA_DIR") {
        return Some(PathBuf::from(d));
    }
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Locus"))
}

impl Settings {
    pub fn load() -> Self {
        crate::datafile::load_named::<Settings>("settings.json")
            .map(|mut s| {
                s.migrate();
                s
            })
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let dir = app_dir().ok_or_else(|| std::io::Error::other("APPDATA is not set"))?;
        std::fs::create_dir_all(&dir)?;
        write_atomic(
            &dir.join("settings.json"),
            &serde_json::to_vec_pretty(self)?,
        )
    }
}

/// Write to a temp file and rename, so a crash never leaves half a file.
/// Refuses a file that was unreadable at start-up (see `datafile`).
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if crate::datafile::is_blocked(path) {
        return Err(crate::datafile::refused(path));
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Maps the OS locale (`zh-TW`, `zh-Hant-HK`, `en-US`, ...) to a supported
/// language. Traditional Chinese (`zh-TW`, `zh-HK`, `zh-MO`, `zh-Hant*`) gets
/// `zh-TW`, every other Chinese (`zh-CN`, `zh-SG`, `zh-Hans*`, plain `zh`) gets
/// `zh-CN`, everything else `en`.
pub fn system_language() -> &'static str {
    let loc = sys_locale::get_locale().unwrap_or_default();
    pick_language(&loc)
}

fn pick_language(loc: &str) -> &'static str {
    let l = loc.to_ascii_lowercase().replace('_', "-");
    let zh = l == "zh" || l.starts_with("zh-");
    if !zh {
        "en"
    } else if l.contains("hans") {
        "zh-CN"
    } else if l.contains("hant") || l.contains("-tw") || l.contains("-hk") || l.contains("-mo") {
        "zh-TW"
    } else {
        "zh-CN"
    }
}

pub fn normalize(lang: &str) -> &'static str {
    LANGUAGES
        .iter()
        .copied()
        .find(|l| l.eq_ignore_ascii_case(lang))
        .unwrap_or_else(|| pick_language(lang))
}

/// Writes a user file through a temp file in the same folder, so a failed
/// write never leaves half a file.
pub fn write_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("wbtmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl Settings {
    /// Row limit for new tabs (`None` = unlimited).
    pub fn default_limit(&self) -> Option<u64> {
        match self.row_limit {
            None => Some(crate::spaces::DEFAULT_ROW_LIMIT),
            Some(0) => None,
            Some(n) => Some(n),
        }
    }
}

impl Settings {
    /// Rows kept per query before it is stopped (`u64::MAX` = no cap).
    pub fn rows_cap(&self) -> u64 {
        match self.max_rows {
            None => 1_000_000,
            Some(0) => u64::MAX,
            Some(n) => n,
        }
    }

    pub fn page_idle(&self) -> std::time::Duration {
        std::time::Duration::from_secs(60 * self.page_idle_minutes.unwrap_or(10).max(1))
    }
}

/// Choices of the translucency setting: how much more see-through than the default.
pub const TRANSLUCENCY_CHOICES: [f32; 4] = [0.0, 0.1, 0.2, 0.3];

/// Nearest choice; a tie goes to the lower (more readable) one. Maps the old
/// 0.15 / 0.35 / 0.55 / 0.75 values to 0.1 / 0.3 / 0.3 / 0.3.
pub fn snap_translucency(v: f32) -> f32 {
    let mut best = TRANSLUCENCY_CHOICES[0];
    for c in TRANSLUCENCY_CHOICES {
        if (c - v).abs() < (best - v).abs() - 1e-6 {
            best = c;
        }
    }
    best
}

/// Window materials in the order of the setting (the first is the default).
pub const MATERIALS: [&str; 4] = ["mica", "blur", "mica_alt", "opaque"];

/// Alpha of the window gradient (sidebar, title bar, gutter) for a material:
/// dark .78 blur / .82 mica, light .82 / .86, minus the translucency choice.
/// Opaque = 1. Everything else in the window is opaque whatever this says.
pub fn glass_alpha(material: &str, dark: bool, translucency: f32) -> f32 {
    let base = match (material, dark) {
        ("opaque", _) => return 1.0,
        ("blur", true) => 0.78,
        ("blur", false) => 0.82,
        (_, true) => 0.82,
        (_, false) => 0.86,
    };
    (base - snap_translucency(translucency)).clamp(0.4, 1.0)
}

impl Settings {
    /// The material in use: a missing or unknown value is Opaque (glass is postponed).
    pub fn material(&self) -> &'static str {
        MATERIALS
            .iter()
            .copied()
            .find(|m| Some(*m) == self.material.as_deref())
            .unwrap_or("opaque")
    }

    /// Translucency choice in use (missing = 0, old values migrated).
    pub fn translucency_value(&self) -> f32 {
        snap_translucency(self.translucency.unwrap_or(0.0))
    }

    /// Rewrites old saved values to the current choices (done by `load`).
    pub fn migrate(&mut self) {
        if let Some(t) = self.translucency {
            self.translucency = Some(snap_translucency(t));
        }
    }
}

impl Settings {
    /// "Space tint" setting: 0 = none, 1 = the mockup's gradient, 2 = double.
    pub fn space_tint(&self) -> f32 {
        self.space_tint.unwrap_or(1.0).clamp(0.0, 2.0)
    }
}

impl Settings {
    /// Zebra striping of the result grid.
    pub fn grid_stripes(&self) -> bool {
        self.grid_stripes.unwrap_or(true)
    }
}

impl Settings {
    /// "Confirm writes on non-Production connections" (Production always confirms).
    pub fn confirm_nonprod(&self) -> bool {
        self.confirm_nonprod_writes.unwrap_or(true)
    }

    pub fn prod_manual_tx(&self) -> bool {
        self.prod_manual_tx.unwrap_or(true)
    }

    /// Zero = no idle warning.
    pub fn tx_idle(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.tx_idle_minutes.unwrap_or(10) * 60)
    }
}

impl Settings {
    /// Options of "Format SQL" (keyword case, indent width).
    pub fn format_options(&self) -> db_core::format::FormatOptions {
        db_core::format::FormatOptions {
            uppercase_keywords: self.format_uppercase.unwrap_or(true),
            indent: match self.format_indent {
                Some(2) => 2,
                _ => 4,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn locales() {
        assert_eq!(pick_language("zh-TW"), "zh-TW");
        assert_eq!(pick_language("zh-Hant-HK"), "zh-TW");
        for l in [
            "zh-CN",
            "zh_CN",
            "zh-SG",
            "zh-Hans",
            "zh-Hans-CN",
            "zh-Hans-HK",
            "zh",
        ] {
            assert_eq!(pick_language(l), "zh-CN", "{l}");
        }
        for l in ["zh-TW", "zh-HK", "zh-MO", "zh-Hant", "zh-Hant-TW", "zh_TW"] {
            assert_eq!(pick_language(l), "zh-TW", "{l}");
        }
        for l in ["en-US", "en", "ja-JP", "", "zhuang"] {
            assert_eq!(pick_language(l), "en", "{l}");
        }
        assert_eq!(normalize("ZH-tw"), "zh-TW");
        assert_eq!(normalize("zh-cn"), "zh-CN");
    }

    #[test]
    fn translucency_migration() {
        assert_eq!(snap_translucency(0.15), 0.1);
        assert_eq!(snap_translucency(0.35), 0.3);
        assert_eq!(snap_translucency(0.55), 0.3);
        assert_eq!(snap_translucency(0.75), 0.3);
        assert_eq!(snap_translucency(0.0), 0.0);
        assert_eq!(snap_translucency(0.2), 0.2);
        let mut s: Settings =
            serde_json::from_str(r#"{"material":"blur","translucency":0.55}"#).unwrap();
        s.migrate();
        assert_eq!(s.translucency, Some(0.3));
        assert_eq!(s.material(), "blur");
    }

    #[test]
    fn material_default_is_opaque() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.material(), "opaque");
        assert_eq!(s.translucency_value(), 0.0);
        let s: Settings = serde_json::from_str(r#"{"material":"mica"}"#).unwrap();
        assert_eq!(s.material(), "mica");
        let s: Settings = serde_json::from_str(r#"{"material":"nonsense"}"#).unwrap();
        assert_eq!(s.material(), "opaque");
    }

    #[test]
    fn glass_alphas() {
        assert_eq!(glass_alpha("opaque", true, 0.3), 1.0);
        assert!((glass_alpha("mica", true, 0.0) - 0.82).abs() < 1e-6);
        assert!((glass_alpha("blur", false, 0.0) - 0.82).abs() < 1e-6);
        assert!((glass_alpha("mica_alt", false, 0.1) - 0.76).abs() < 1e-6);
        assert!((glass_alpha("blur", true, 0.3) - 0.48).abs() < 1e-6);
    }
}

#[cfg(test)]
mod stripe_tests {
    use super::*;

    #[test]
    fn grid_stripes_default_on_and_round_trip() {
        assert!(Settings::default().grid_stripes());
        let off = Settings {
            grid_stripes: Some(false),
            ..Default::default()
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&off).unwrap()).unwrap();
        assert!(!back.grid_stripes());
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert!(old.grid_stripes());
    }
}
