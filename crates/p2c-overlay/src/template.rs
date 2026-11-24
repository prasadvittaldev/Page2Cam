//! Overlay templates: named, parameterised graphics. A template is HTML that
//! runs in the sandboxed layer frame together with the `P2C` runtime
//! ([`RUNTIME_JS`]); its parameters are JSON data, checked here against
//! their declared types before they reach the frame.
use std::sync::OnceLock;

use anyhow::{anyhow, bail, ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::util::normalize_colour;

/// Largest template HTML (bytes).
pub const MAX_TEMPLATE_HTML: usize = 256 * 1024;
/// Largest data URI an `asset` parameter takes (characters).
pub const MAX_DATA_URI: usize = 1024 * 1024 + 64 * 1024;
/// Largest parameter set of one layer (bytes of JSON).
pub const MAX_PARAMS_BYTES: usize = 2 * 1024 * 1024;
/// Longest template id.
pub const MAX_TEMPLATE_ID: usize = 40;
/// Most parameters of one template.
pub const MAX_PARAMS: usize = 40;

/// The `P2C` runtime every template page loads first.
pub const RUNTIME_JS: &str = include_str!("../templates/_runtime.js");

/// Template categories: `(id, label)`.
pub const CATEGORIES: &[(&str, &str)] = &[
    ("basics", "Basics"),
    ("time", "Time"),
    ("cards", "Full-frame cards"),
    ("lists", "Lists and text"),
    ("data", "Data"),
    ("cta", "Call to action"),
    ("calm", "Calm"),
    ("software", "Software companies"),
    ("mine", "My templates"),
];

/// Looks every template can take.
pub const THEMES: &[&str] = &["brand", "news", "minimal"];

/// A parameter's type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    /// One line of text (≤ 500 characters).
    Text,
    /// Several lines (≤ 5000 characters).
    Longtext,
    /// A number (clamped to min/max).
    Number,
    Bool,
    /// `#rrggbb`, or empty for the look's own colour.
    Colour,
    /// One of `options`.
    Choice,
    /// A list of texts.
    List,
    /// Rows of text cells (`columns` name them).
    Table,
    /// `YYYY-MM-DDTHH:MM[:SS][Z|±hh:mm]` or `HH:MM` (today, local time).
    Datetime,
    /// Seconds (a number, or `m:ss` / `h:mm:ss`).
    Duration,
    /// An http(s) address.
    Url,
    /// A picture: an http(s) address or a `data:image/…;base64,` URI.
    Asset,
    /// Text or a link to show as a QR code.
    Qr,
}

impl ParamType {
    pub fn as_str(self) -> &'static str {
        match self {
            ParamType::Text => "text",
            ParamType::Longtext => "longtext",
            ParamType::Number => "number",
            ParamType::Bool => "bool",
            ParamType::Colour => "colour",
            ParamType::Choice => "choice",
            ParamType::List => "list",
            ParamType::Table => "table",
            ParamType::Datetime => "datetime",
            ParamType::Duration => "duration",
            ParamType::Url => "url",
            ParamType::Asset => "asset",
            ParamType::Qr => "qr",
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One parameter of a template.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ParamType,
    /// The value when none is given (`null` = the type's empty value).
    #[serde(default)]
    pub default: Value,
    #[serde(default)]
    pub help: String,
    /// Must have a non-empty value (given, or the default).
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Choices of a `choice` parameter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// Column names of a `table` parameter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
    /// Most entries of a `list` or rows of a `table` (default 50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<usize>,
}

fn default_rect() -> [f32; 4] {
    [0.6, 0.05, 0.35, 0.35]
}

/// A template: meta data plus its HTML.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default = "mine")]
    pub category: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub params: Vec<Param>,
    #[serde(default)]
    pub html: String,
    /// Default geometry `[x, y, w, h]` (0..1 of the output).
    #[serde(default = "default_rect")]
    pub rect: [f32; 4],
    /// Covers the whole output (a card): default geometry 0,0,1,1.
    #[serde(default, skip_serializing_if = "is_false")]
    pub full_frame: bool,
    /// Example parameters (what the guide and the control page show).
    #[serde(default)]
    pub example: Map<String, Value>,
    /// An example live change (`update_layer {params}`).
    #[serde(default)]
    pub update: Map<String, Value>,
    #[serde(default)]
    pub builtin: bool,
}

fn mine() -> String {
    "mine".into()
}

/// The parameters every template takes.
fn common_params() -> Vec<Param> {
    vec![
        Param {
            name: "theme".into(),
            kind: ParamType::Choice,
            default: json!("brand"),
            help: "Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading)".into(),
            required: false,
            min: None,
            max: None,
            options: THEMES.iter().map(|s| s.to_string()).collect(),
            columns: Vec::new(),
            max_items: None,
        },
        Param {
            name: "accent".into(),
            kind: ParamType::Colour,
            default: json!(""),
            help: "Accent colour #rrggbb (empty: the look's own)".into(),
            required: false,
            min: None,
            max: None,
            options: Vec::new(),
            columns: Vec::new(),
            max_items: None,
        },
    ]
}

/// A template id: `a-z0-9-`, 1..=40, no leading/trailing `-`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_TEMPLATE_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}

fn is_empty_value(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

fn check_url(s: &str, what: &str) -> Result<()> {
    let l = s.trim().to_ascii_lowercase();
    ensure!(
        l.starts_with("https://") || l.starts_with("http://"),
        "{what} must be an http:// or https:// address"
    );
    ensure!(s.len() <= 2048, "{what} is longer than 2048 characters");
    ensure!(
        !s.contains(['"', '<', '>', '\\', ' ', '\n', '\r', '\t']),
        "{what} contains characters that are not allowed"
    );
    Ok(())
}

fn check_text(v: &Value, what: &str, max: usize) -> Result<String> {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => bail!("{what} must be text"),
    };
    ensure!(
        s.chars().count() <= max,
        "{what} is longer than {max} characters"
    );
    Ok(s)
}

/// Seconds from a number or `m:ss` / `h:mm:ss`.
fn parse_duration(v: &Value, what: &str) -> Result<f64> {
    let secs = match v {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let parts: Vec<&str> = s.trim().split(':').collect();
            ensure!(
                (1..=3).contains(&parts.len()),
                "{what} must be seconds or m:ss / h:mm:ss"
            );
            let mut t = 0.0;
            for p in parts {
                let n: f64 = p
                    .trim()
                    .parse()
                    .map_err(|_| anyhow!("{what} must be seconds or m:ss / h:mm:ss"))?;
                ensure!(n >= 0.0, "{what} cannot be negative");
                t = t * 60.0 + n;
            }
            t
        }
        _ => bail!("{what} must be seconds or m:ss / h:mm:ss"),
    };
    ensure!(
        secs.is_finite() && (0.0..=7.0 * 86400.0).contains(&secs),
        "{what} must be 0 to 7 days"
    );
    Ok(secs.round())
}

fn digits(s: &str, n: usize) -> Option<u32> {
    (s.len() == n && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
}

/// `HH:MM` or an ISO date-time `YYYY-MM-DDTHH:MM[:SS][.fff][Z|±hh:mm]`.
fn check_datetime(s: &str, what: &str) -> Result<()> {
    let bad = || anyhow!("{what} must be like 2026-12-31T18:30, 2026-12-31T18:30:00Z or 18:30");
    let s = s.trim();
    let hm = |t: &str| -> Option<()> {
        let mut it = t.split(':');
        let h = digits(it.next()?, 2)?;
        let m = digits(it.next()?, 2)?;
        if let Some(sec) = it.next() {
            let whole = sec.split('.').next()?;
            if digits(whole, 2)? > 59 {
                return None;
            }
        }
        (h < 24 && m < 60 && it.next().is_none()).then_some(())
    };
    if !s.contains('T') {
        return hm(s).ok_or_else(bad);
    }
    let (date, time) = s.split_once('T').ok_or_else(bad)?;
    let mut d = date.split('-');
    let ok_date = (|| {
        let _y = digits(d.next()?, 4)?;
        let m = digits(d.next()?, 2)?;
        let day = digits(d.next()?, 2)?;
        ((1..=12).contains(&m) && (1..=31).contains(&day) && d.next().is_none()).then_some(())
    })();
    ok_date.ok_or_else(bad)?;
    let (clock, zone) = if let Some(t) = time.strip_suffix('Z') {
        (t, "")
    } else if let Some(i) = time.rfind(['+', '-']) {
        (&time[..i], &time[i + 1..])
    } else {
        (time, "")
    };
    hm(clock).ok_or_else(bad)?;
    if !zone.is_empty() {
        hm(zone).ok_or_else(bad)?;
    }
    Ok(())
}

fn check_asset(s: &str, what: &str) -> Result<()> {
    if s.trim().is_empty() {
        return Ok(());
    }
    let l = s.get(..32).unwrap_or(s).to_ascii_lowercase();
    if l.starts_with("data:") {
        let ok = ["png", "jpeg", "jpg", "webp", "gif", "svg+xml"]
            .iter()
            .any(|t| l.starts_with(&format!("data:image/{t};base64,")));
        ensure!(
            ok,
            "{what}: only data:image/(png|jpeg|webp|gif|svg+xml);base64 pictures"
        );
        ensure!(s.len() <= MAX_DATA_URI, "{what} is larger than 1 MB");
        ensure!(
            s.bytes()
                .all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'<' | b'>' | b'\\')),
            "{what} is not a valid data URI"
        );
        return Ok(());
    }
    check_url(s, what)
}

impl Param {
    /// The value used when none is given.
    pub fn default_value(&self) -> Value {
        if !self.default.is_null() {
            return self.default.clone();
        }
        match self.kind {
            ParamType::Number => json!(self.min.unwrap_or(0.0).max(0.0)),
            ParamType::Duration => json!(0),
            ParamType::Bool => json!(false),
            ParamType::List | ParamType::Table => json!([]),
            ParamType::Choice => json!(self.options.first().cloned().unwrap_or_default()),
            _ => json!(""),
        }
    }

    /// Check one value against the type; the value as stored.
    pub fn check(&self, v: &Value) -> Result<Value> {
        let what = &self.name;
        Ok(match self.kind {
            ParamType::Text => json!(check_text(v, what, 500)?),
            ParamType::Longtext => json!(check_text(v, what, 5000)?),
            ParamType::Number => {
                let n = match v {
                    Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
                    Value::String(s) => s
                        .trim()
                        .parse::<f64>()
                        .map_err(|_| anyhow!("{what} must be a number"))?,
                    _ => bail!("{what} must be a number"),
                };
                ensure!(n.is_finite(), "{what} must be a finite number");
                let n = n.clamp(self.min.unwrap_or(-1e12), self.max.unwrap_or(1e12));
                json!(n)
            }
            ParamType::Bool => match v {
                Value::Bool(b) => json!(b),
                _ => bail!("{what} must be true or false"),
            },
            ParamType::Colour => {
                let s = check_text(v, what, 20)?;
                if s.trim().is_empty() {
                    json!("")
                } else {
                    json!(normalize_colour(&s).map_err(|e| anyhow!("{what}: {e}"))?)
                }
            }
            ParamType::Choice => {
                let s = check_text(v, what, 100)?;
                ensure!(
                    self.options.contains(&s),
                    "{what} must be one of {}",
                    self.options.join(", ")
                );
                json!(s)
            }
            ParamType::List => {
                let max = self.max_items.unwrap_or(50);
                let a = v
                    .as_array()
                    .ok_or_else(|| anyhow!("{what} must be a list of texts"))?;
                ensure!(a.len() <= max, "{what} has more than {max} entries");
                let mut out = Vec::new();
                for i in a {
                    out.push(json!(check_text(i, &format!("an entry of {what}"), 500)?));
                }
                Value::Array(out)
            }
            ParamType::Table => {
                let max = self.max_items.unwrap_or(50);
                let cols = self.columns.len().max(1);
                let rows = v.as_array().ok_or_else(|| {
                    anyhow!(
                        "{what} must be a list of rows, each a list of cells ({})",
                        self.columns.join(", ")
                    )
                })?;
                ensure!(rows.len() <= max, "{what} has more than {max} rows");
                let mut out = Vec::new();
                for r in rows {
                    let cells = r.as_array().ok_or_else(|| {
                        anyhow!(
                            "each row of {what} must be a list of cells ({})",
                            self.columns.join(", ")
                        )
                    })?;
                    ensure!(
                        cells.len() <= cols,
                        "a row of {what} has more than {cols} cells ({})",
                        self.columns.join(", ")
                    );
                    let mut row = Vec::new();
                    for c in cells {
                        let c = if c.is_null() { json!("") } else { c.clone() };
                        row.push(json!(check_text(&c, &format!("a cell of {what}"), 200)?));
                    }
                    out.push(Value::Array(row));
                }
                Value::Array(out)
            }
            ParamType::Datetime => {
                let s = check_text(v, what, 40)?;
                if !s.trim().is_empty() {
                    check_datetime(&s, what)?;
                }
                json!(s.trim())
            }
            ParamType::Duration => json!(parse_duration(v, what)? as u64),
            ParamType::Url => {
                let s = check_text(v, what, 2048)?;
                if !s.trim().is_empty() {
                    check_url(&s, what)?;
                }
                json!(s.trim())
            }
            ParamType::Asset => {
                let s = match v {
                    Value::String(s) => s.clone(),
                    _ => bail!("{what} must be an http(s) address or a data:image URI"),
                };
                check_asset(&s, what)?;
                json!(s.trim())
            }
            ParamType::Qr => json!(check_text(v, what, 1000)?),
        })
    }
}

impl Template {
    /// Its parameters plus the common ones (`theme`, `accent`), unless it
    /// declares those itself.
    pub fn all_params(&self) -> Vec<Param> {
        let mut v = self.params.clone();
        for c in common_params() {
            if !v.iter().any(|p| p.name == c.name) {
                v.push(c);
            }
        }
        v
    }

    fn param(&self, name: &str) -> Option<Param> {
        self.all_params().into_iter().find(|p| p.name == name)
    }

    /// Default geometry `(x, y, w, h)`.
    pub fn geometry(&self) -> (f32, f32, f32, f32) {
        if self.full_frame {
            (0.0, 0.0, 1.0, 1.0)
        } else {
            let [x, y, w, h] = self.rect;
            (x, y, w, h)
        }
    }

    /// Check given parameters (all of them, not partial). Unknown names
    /// are refused; required ones must end up non-empty. The values as
    /// stored (numbers clamped, colours normalised, durations in seconds).
    pub fn check_params(&self, given: &Map<String, Value>) -> Result<Map<String, Value>> {
        let mut out = Map::new();
        for (k, v) in given {
            let p = self.param(k).ok_or_else(|| {
                anyhow!(
                    "unknown param {k:?} for template {:?} (params: {})",
                    self.id,
                    self.all_params()
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            if v.is_null() {
                continue;
            }
            out.insert(k.clone(), p.check(v)?);
        }
        for p in self.all_params().iter().filter(|p| p.required) {
            let v = out
                .get(&p.name)
                .cloned()
                .unwrap_or_else(|| p.default_value());
            ensure!(
                !is_empty_value(&v),
                "param {:?} is required for template {:?} ({})",
                p.name,
                self.id,
                p.help
            );
        }
        let size = serde_json::to_string(&out).map_or(0, |s| s.len());
        ensure!(size <= MAX_PARAMS_BYTES, "params are larger than 2 MB");
        Ok(out)
    }

    /// What the frame gets: every parameter (given or default); `qr`
    /// values become `{text, n, d}` (module count and SVG path).
    pub fn effective(&self, given: &Map<String, Value>) -> Map<String, Value> {
        let mut out = Map::new();
        for p in self.all_params() {
            let v = given
                .get(&p.name)
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| p.default_value());
            let v = if p.kind == ParamType::Qr {
                let text = v.as_str().unwrap_or_default().to_string();
                match crate::qr::svg_path(&text) {
                    Ok((n, d)) if !text.is_empty() => json!({ "text": text, "n": n, "d": d }),
                    _ => json!({ "text": text, "n": 0, "d": "" }),
                }
            } else {
                v
            };
            out.insert(p.name, v);
        }
        out
    }

    /// Check the definition itself (ids, names, types, defaults, examples,
    /// HTML size).
    pub fn validate_meta(&self) -> Result<()> {
        ensure!(
            valid_id(&self.id),
            "template id {:?} must be 1 to 40 of a-z, 0-9 and -",
            self.id
        );
        ensure!(
            !self.name.trim().is_empty() && self.name.chars().count() <= 80,
            "a template needs a name of 1 to 80 characters"
        );
        ensure!(
            valid_id(&self.category),
            "category {:?} must be a-z, 0-9 and -",
            self.category
        );
        ensure!(
            self.description.chars().count() <= 500,
            "description is longer than 500 characters"
        );
        ensure!(!self.html.trim().is_empty(), "a template needs html");
        ensure!(
            self.html.len() <= MAX_TEMPLATE_HTML,
            "template html is larger than 256 KB"
        );
        ensure!(
            self.params.len() <= MAX_PARAMS,
            "at most {MAX_PARAMS} params"
        );
        for v in self.rect {
            ensure!((0.0..=1.0).contains(&v), "rect values must be 0..1");
        }
        ensure!(
            self.rect[2] >= 0.01 && self.rect[3] >= 0.01,
            "rect is too small"
        );
        let mut seen = Vec::new();
        for p in &self.params {
            ensure!(
                !p.name.is_empty()
                    && p.name.len() <= 40
                    && p.name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "param name {:?} must be a-z, 0-9 and _",
                p.name
            );
            ensure!(
                !seen.contains(&p.name),
                "param {:?} is listed twice",
                p.name
            );
            seen.push(p.name.clone());
            ensure!(
                p.help.chars().count() <= 300,
                "help of {:?} is too long",
                p.name
            );
            if p.kind == ParamType::Choice {
                ensure!(
                    !p.options.is_empty(),
                    "choice param {:?} needs options",
                    p.name
                );
            }
            if !p.default.is_null() {
                p.check(&p.default)
                    .map_err(|e| anyhow!("default of {:?}: {e}", p.name))?;
            }
        }
        if !self.example.is_empty() {
            self.check_params(&self.example)
                .map_err(|e| anyhow!("example: {e}"))?;
        }
        if !self.update.is_empty() {
            let mut merged = self.example.clone();
            merged.extend(self.update.clone());
            self.check_params(&merged)
                .map_err(|e| anyhow!("update: {e}"))?;
        }
        Ok(())
    }

    /// A short line for lists.
    pub fn summary_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "category": self.category,
            "description": self.description,
            "builtin": self.builtin,
            "full_frame": self.full_frame,
        })
    }

    /// The full definition as JSON (`html` included when `with_html`).
    pub fn to_json(&self, with_html: bool) -> Value {
        let mut v = serde_json::to_value(self).expect("a template serializes");
        if let Some(m) = v.as_object_mut() {
            if !with_html {
                m.remove("html");
            }
            m.insert(
                "all_params".into(),
                serde_json::to_value(self.all_params()).unwrap_or_default(),
            );
        }
        v
    }

    /// Parse a definition (an export file, an agent's or the control
    /// page's) as a user template, and check it.
    pub fn from_user_json(v: Value) -> Result<Template> {
        let mut m = match v {
            Value::Object(m) => m,
            _ => bail!("a template is a JSON object"),
        };
        // Export files and full answers carry these; they are ours to set.
        m.remove("all_params");
        m.remove("builtin");
        m.remove("format");
        let mut t: Template =
            serde_json::from_value(Value::Object(m)).map_err(|e| anyhow!("bad template: {e}"))?;
        t.builtin = false;
        t.validate_meta()?;
        Ok(t)
    }
}

/// The built-in templates (embedded at build time from `templates/`).
pub fn builtins() -> &'static [Template] {
    static ALL: OnceLock<Vec<Template>> = OnceLock::new();
    ALL.get_or_init(|| {
        crate::builtin::SOURCES
            .iter()
            .map(|(id, meta, html)| {
                let mut t: Template = serde_json::from_str(meta)
                    .unwrap_or_else(|e| panic!("templates/{id}.json: {e}"));
                t.id = id.to_string();
                t.html = html.to_string();
                t.builtin = true;
                t
            })
            .collect()
    })
}

/// A built-in template by id.
pub fn builtin(id: &str) -> Option<&'static Template> {
    builtins().iter().find(|t| t.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tpl() -> Template {
        serde_json::from_value(json!({
            "id": "t", "name": "T", "category": "basics", "html": "<b>x</b>",
            "params": [
                {"name": "name", "type": "text", "required": true, "help": "who"},
                {"name": "n", "type": "number", "min": 0, "max": 10},
                {"name": "link", "type": "url"},
                {"name": "logo", "type": "asset"},
                {"name": "c", "type": "colour"},
                {"name": "d", "type": "duration"},
                {"name": "at", "type": "datetime"},
                {"name": "rows", "type": "table", "columns": ["a", "b"]},
                {"name": "q", "type": "qr"},
                {"name": "pick", "type": "choice", "options": ["x", "y"]}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn every_builtin_is_valid() {
        let all = builtins();
        assert!(all.len() >= 40, "{} built-ins", all.len());
        for t in all {
            t.validate_meta()
                .unwrap_or_else(|e| panic!("{}: {e:#}", t.id));
            assert!(
                CATEGORIES.iter().any(|(c, _)| *c == t.category) && t.category != "mine",
                "{}: category {}",
                t.id,
                t.category
            );
            assert!(!t.example.is_empty(), "{}: no example", t.id);
            assert!(!t.update.is_empty(), "{}: no update example", t.id);
            assert!(!t.description.is_empty(), "{}: no description", t.id);
            for bad in [
                "innerHTML",
                "outerHTML",
                "insertAdjacentHTML",
                "document.write",
                "eval(",
            ] {
                assert!(!t.html.contains(bad), "{}: uses {bad}", t.id);
            }
            assert!(
                t.html.contains("P2C.onParams"),
                "{}: never reads its params",
                t.id
            );
        }
        for (c, _) in CATEGORIES.iter().filter(|(c, _)| *c != "mine") {
            assert!(all.iter().any(|t| t.category == *c), "no template in {c}");
        }
        for bad in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "eval(",
        ] {
            assert!(!RUNTIME_JS.contains(bad), "runtime uses {bad}");
        }
    }

    #[test]
    fn params_are_checked() {
        let t = tpl();
        let ok = |v: Value| t.check_params(v.as_object().unwrap());
        let err = |v: Value| ok(v).unwrap_err().to_string();
        assert!(err(json!({})).contains("\"name\" is required"));
        assert!(err(json!({"name": "a", "nope": 1})).contains("unknown param \"nope\""));
        let r = ok(json!({"name": "</script><b>", "n": 99, "d": "1:30", "c": "#ABCDEF"})).unwrap();
        assert_eq!(r["name"], "</script><b>", "text is data");
        assert_eq!(r["n"], 10.0);
        assert_eq!(r["d"], 90);
        assert_eq!(r["c"], "#abcdef");
        assert!(err(json!({"name": "a", "link": "javascript:alert(1)"})).contains("http"));
        assert!(
            err(json!({"name": "a", "logo": "data:text/html;base64,AAAA"})).contains("data:image")
        );
        let big = format!("data:image/png;base64,{}", "A".repeat(MAX_DATA_URI));
        assert!(err(json!({"name": "a", "logo": big})).contains("1 MB"));
        assert!(ok(json!({"name": "a", "logo": "data:image/png;base64,iVBORw0KGgo="})).is_ok());
        assert!(ok(json!({"name": "a", "logo": "https://example.com/l.png"})).is_ok());
        assert!(err(json!({"name": "a", "c": "#abc"})).contains("colour"));
        assert!(err(json!({"name": "a", "at": "tomorrow"})).contains("must be like"));
        for at in [
            "18:30",
            "2026-12-31T18:30",
            "2026-12-31T18:30:00Z",
            "2026-12-31T18:30:00.5+01:00",
        ] {
            assert!(ok(json!({"name": "a", "at": at})).is_ok(), "{at}");
        }
        assert!(err(json!({"name": "a", "rows": [["1", "2", "3"]]})).contains("more than 2 cells"));
        assert!(
            ok(json!({"name": "a", "rows": [["x", 2]]})).is_ok(),
            "numbers as cells"
        );
        assert!(err(json!({"name": "a", "pick": "z"})).contains("one of x, y"));
        assert!(err(json!({"name": "a", "theme": "pink"})).contains("brand"));
        assert!(err(json!({"name": "a", "n": "lots"})).contains("number"));
    }

    #[test]
    fn effective_fills_defaults_and_qr() {
        let t = tpl();
        let e = t.effective(
            json!({"name": "A", "q": "https://example.com"})
                .as_object()
                .unwrap(),
        );
        assert_eq!(e["name"], "A");
        assert_eq!(e["n"], 0.0);
        assert_eq!(e["theme"], "brand");
        assert_eq!(e["accent"], "");
        assert_eq!(e["rows"], json!([]));
        assert_eq!(e["pick"], "x");
        assert!(e["q"]["n"].as_u64().unwrap() >= 21);
        assert!(e["q"]["d"].as_str().unwrap().starts_with('M'));
        let none = t.effective(&Map::new());
        assert_eq!(none["q"]["n"], 0);
    }

    #[test]
    fn user_json_is_checked() {
        let e = Template::from_user_json(json!({"id": "x", "name": "X", "html": "<b>", "evil": 1}))
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown field"), "{e}");
        let e = Template::from_user_json(json!({"id": "Bad Id", "name": "X", "html": "<b>"}))
            .unwrap_err()
            .to_string();
        assert!(e.contains("a-z"), "{e}");
        let t = Template::from_user_json(
            json!({"id": "x", "name": "X", "html": "<b>", "builtin": true}),
        )
        .unwrap();
        assert!(!t.builtin);
        assert_eq!(t.category, "mine");
    }
}
