//! User themes stored in `themes.json` next to `state.json`, merged with the
//! built-in set into one list the THEME page draws from.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::storage;
use crate::theme::{self, Background, Category, Look, Palette, Rgb, Seeds};

pub const NAME_MAX: usize = 16;
const DEFAULT_TINT: f32 = 0.34;

fn default_tint() -> f32 {
    DEFAULT_TINT
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hex(pub Rgb);

impl Serialize for Hex {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format_hex(self.0))
    }
}

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        parse_hex(&text).map(Hex).ok_or_else(|| {
            serde::de::Error::custom(format!("invalid colour \"{text}\", expected #RRGGBB"))
        })
    }
}

pub fn parse_hex(text: &str) -> Option<Rgb> {
    let text = text.trim();
    let digits = text.strip_prefix('#').unwrap_or(text);
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

pub fn format_hex(rgb: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThemeSpec {
    pub name: String,
    /// 1 colour = solid, 2 = top-left to bottom-right blend,
    /// 4 = corners (top-left, top-right, bottom-right, bottom-left).
    pub background: Vec<Hex>,
    pub accent: Hex,
    #[serde(default = "default_tint")]
    pub tint: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dark: Option<bool>,
    #[serde(default, skip_serializing_if = "Overrides::is_empty")]
    pub colors: Overrides,
}

impl ThemeSpec {
    fn from_look(name: String, look: &Look) -> Self {
        let background = match look.gradient {
            Some(corners) => corners.into_iter().map(Hex).collect(),
            None => vec![Hex(look.palette.bg)],
        };
        Self {
            name,
            background,
            accent: Hex(look.palette.ink_tint),
            // Generated looks carry long fractions; the editor steps by 0.02.
            tint: (look.palette.ink_mix * 100.0).round() / 100.0,
            dark: None,
            colors: Overrides::default(),
        }
    }

    pub fn background(&self) -> Option<Background> {
        match self.background.as_slice() {
            [Hex(solid)] => Some(Background::Solid(*solid)),
            [Hex(from), Hex(to)] => Some(Background::Diagonal(*from, *to)),
            [Hex(tl), Hex(tr), Hex(br), Hex(bl)] => Some(Background::Corners([*tl, *tr, *br, *bl])),
            _ => None,
        }
    }

    pub fn tint(&self) -> f32 {
        if self.tint.is_finite() {
            self.tint.clamp(0.0, 1.0)
        } else {
            DEFAULT_TINT
        }
    }

    pub fn look(&self) -> Option<Look> {
        let seeds = Seeds {
            background: self.background()?,
            accent: self.accent.0,
            tint: self.tint(),
            dark: self.dark,
        };
        let mut look = theme::derive(&seeds);
        self.colors.apply(&mut look.palette);
        Some(look)
    }

    /// Switches between 1, 2 and 4 background colours while keeping the
    /// overall look close to what it was.
    pub fn set_background_count(&mut self, count: usize) {
        let Some(background) = self.background() else {
            return;
        };
        let corners = background.corners();
        let colors: Vec<Rgb> = match count {
            1 => vec![theme::average(&corners)],
            2 => vec![corners[0], corners[2]],
            _ => corners.to_vec(),
        };
        self.background = colors.into_iter().map(Hex).collect();
    }
}

/// Explicit palette slots that win over the derived ones.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Overrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<Hex>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg_deep: Option<Hex>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<Hex>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_hi: Option<Hex>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_sunken: Option<Hex>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_hi: Option<Hex>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Hex>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_dim: Option<Hex>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<Hex>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub danger: Option<Hex>,
}

impl Overrides {
    fn slots(&self) -> [Option<Hex>; 10] {
        [
            self.bg,
            self.bg_deep,
            self.surface,
            self.surface_hi,
            self.surface_sunken,
            self.text_hi,
            self.text,
            self.text_dim,
            self.line,
            self.danger,
        ]
    }

    pub fn count(&self) -> usize {
        self.slots().iter().flatten().count()
    }

    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }

    fn apply(&self, palette: &mut Palette) {
        let targets = [
            &mut palette.bg,
            &mut palette.bg_deep,
            &mut palette.surface,
            &mut palette.surface_hi,
            &mut palette.surface_sunken,
            &mut palette.text_hi,
            &mut palette.text,
            &mut palette.text_dim,
            &mut palette.line,
            &mut palette.danger,
        ];
        for (slot, target) in self.slots().into_iter().zip(targets) {
            if let Some(Hex(rgb)) = slot {
                *target = rgb;
            }
        }
    }
}

#[derive(Deserialize)]
struct ThemeFile {
    #[serde(default)]
    themes: Vec<ThemeSpec>,
}

#[derive(Serialize)]
struct ThemeFileRef<'a> {
    themes: &'a [ThemeSpec],
}

#[derive(Clone, Debug)]
pub struct ThemeEntry {
    pub name: String,
    pub category: Category,
    pub look: Look,
    /// Built-in index, which doubles as the legacy `theme` value in state.json.
    pub builtin: Option<usize>,
    /// Position in `themes.json` for user themes.
    pub spec: Option<usize>,
}

pub struct ThemeLibrary {
    specs: Vec<ThemeSpec>,
    entries: Vec<ThemeEntry>,
    warnings: Vec<String>,
    load_error: Option<String>,
    /// Modification time seen at the last read or write, so a hand edit made
    /// while the app is open is never overwritten silently.
    stamp: Option<SystemTime>,
}

impl ThemeLibrary {
    pub fn path() -> PathBuf {
        storage::data_dir().join("themes.json")
    }

    pub fn load() -> Self {
        let mut library = Self {
            specs: Vec::new(),
            entries: Vec::new(),
            warnings: Vec::new(),
            load_error: None,
            stamp: None,
        };
        library.reload();
        library
    }

    /// Re-reads the file. On a parse error the previous user themes stay
    /// usable but saving is blocked until the file loads cleanly again.
    pub fn reload(&mut self) {
        let path = Self::path();
        match read_specs(&path) {
            Ok(specs) => {
                self.specs = specs;
                self.load_error = None;
            }
            Err(error) => self.load_error = Some(error),
        }
        self.stamp = disk_stamp(&path);
        self.rebuild();
    }

    pub fn save(&mut self) -> Result<(), String> {
        if self.load_error.is_some() {
            return Err("THEMES.JSON NOT SAVED / fix the file, then RELOAD".to_owned());
        }
        let path = Self::path();
        if disk_stamp(&path) != self.stamp {
            return Err("THEMES.JSON CHANGED ON DISK / RELOAD before editing".to_owned());
        }
        let text = serde_json::to_string_pretty(&ThemeFileRef {
            themes: &self.specs,
        })
        .map_err(|error| format!("THEMES.JSON SAVE FAILED / {error}"))?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|error| format!("THEMES.JSON SAVE FAILED / {error}"))?;
        }
        fs::write(&path, text).map_err(|error| format!("THEMES.JSON SAVE FAILED / {error}"))?;
        self.stamp = disk_stamp(&path);
        Ok(())
    }

    pub fn editable(&self) -> bool {
        self.load_error.is_none()
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn entries(&self) -> &[ThemeEntry] {
        &self.entries
    }

    pub fn entry(&self, index: usize) -> Option<&ThemeEntry> {
        self.entries.get(index)
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        let name = name.trim();
        self.entries
            .iter()
            .position(|entry| entry.name.eq_ignore_ascii_case(name))
    }

    pub fn find_builtin(&self, builtin: usize) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.builtin == Some(builtin))
    }

    pub fn spec(&self, spec: usize) -> Option<&ThemeSpec> {
        self.specs.get(spec)
    }

    /// Whether `name` can label a theme; `own` is the user theme being renamed.
    pub fn name_free(&self, name: &str, own: Option<usize>) -> bool {
        let name = name.trim();
        !name.is_empty()
            && !self.entries.iter().any(|entry| {
                (entry.spec.is_none() || entry.spec != own) && entry.name.eq_ignore_ascii_case(name)
            })
    }

    /// Copies an entry into a new user theme and returns the new entry index.
    pub fn duplicate(&mut self, entry: usize) -> Option<usize> {
        let source = self.entries.get(entry)?;
        let name = self.unique_name(&source.name);
        let spec = match source.spec.and_then(|spec| self.specs.get(spec)) {
            Some(spec) => ThemeSpec {
                name: name.clone(),
                ..spec.clone()
            },
            None => ThemeSpec::from_look(name.clone(), &source.look),
        };
        self.specs.push(spec);
        self.rebuild();
        self.find(&name)
    }

    /// Saves a look from outside the library (an AUTO look) as a new user
    /// theme named `base`, or `base 2` and up when taken.
    pub fn adopt(&mut self, base: &str, look: &Look) -> Option<usize> {
        let name = if self.name_free(base, None) {
            base.trim().to_owned()
        } else {
            self.unique_name(base)
        };
        self.specs.push(ThemeSpec::from_look(name.clone(), look));
        self.rebuild();
        self.find(&name)
    }

    /// Applies `change` to a user theme and returns the entry it now sits at.
    pub fn edit(&mut self, spec: usize, change: impl FnOnce(&mut ThemeSpec)) -> Option<usize> {
        change(self.specs.get_mut(spec)?);
        self.rebuild();
        self.entries
            .iter()
            .position(|entry| entry.spec == Some(spec))
    }

    pub fn remove(&mut self, spec: usize) {
        if spec < self.specs.len() {
            self.specs.remove(spec);
            self.rebuild();
        }
    }

    fn unique_name(&self, base: &str) -> String {
        // "AURORA 2" copies to "AURORA 3", not "AURORA 2 2".
        let stem = match base.trim().rsplit_once(' ') {
            Some((head, tail))
                if !head.is_empty() && !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) =>
            {
                head
            }
            _ => base.trim(),
        };
        let mut number = 2;
        loop {
            let suffix = format!(" {number}");
            let room = NAME_MAX.saturating_sub(suffix.chars().count());
            let head: String = stem.chars().take(room).collect();
            let name = format!("{}{suffix}", head.trim_end());
            if self.name_free(&name, None) {
                return name;
            }
            number += 1;
        }
    }

    fn rebuild(&mut self) {
        let mut entries: Vec<ThemeEntry> = (0..theme::BUILTIN_COUNT)
            .map(|index| ThemeEntry {
                name: theme::BUILTIN_NAMES[index].to_owned(),
                category: theme::builtin_category(index),
                look: theme::builtin_look(index),
                builtin: Some(index),
                spec: None,
            })
            .collect();
        // Stable sort: dark before light inside a category, else declared order.
        entries.sort_by_key(|entry| (entry.category, !entry.look.palette.dark));

        self.warnings.clear();
        for (index, spec) in self.specs.iter().enumerate() {
            let name = spec.name.trim();
            let label = if name.is_empty() { "?" } else { name };
            let problem = if name.is_empty() {
                Some("missing name")
            } else if entries
                .iter()
                .any(|entry| entry.name.eq_ignore_ascii_case(name))
            {
                Some("name already used")
            } else if spec.background().is_none() {
                Some("background needs 1, 2 or 4 colours")
            } else {
                None
            };
            match (problem, spec.look()) {
                (None, Some(look)) => entries.push(ThemeEntry {
                    name: name.to_owned(),
                    category: Category::Custom,
                    look,
                    builtin: None,
                    spec: Some(index),
                }),
                (problem, _) => self.warnings.push(format!(
                    "#{} {label}: {}",
                    index + 1,
                    problem.unwrap_or("invalid")
                )),
            }
        }
        self.entries = entries;
    }
}

fn disk_stamp(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

fn read_specs(path: &Path) -> Result<Vec<ThemeSpec>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("READ FAILED / {error}")),
    };
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str::<ThemeFile>(text)
        .map(|file| file.themes)
        .map_err(|error| error.to_string())
}
