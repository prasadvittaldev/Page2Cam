//! The layer list: add, change, show, hide, reorder, remove; auto-hide;
//! persistence; and the state the scene page polls.
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure, Context, Result};
use base64::Engine;
use serde_json::{json, Map, Value};

use crate::assets::{Assets, MediaKind};
use crate::layer::{default_rect, from_json, merge, Content, Layer, KINDS};
use crate::store::TemplateLib;
use crate::template::Template;

/// Most layers at once.
pub const MAX_LAYERS: usize = 32;
/// Longest layer id.
pub const MAX_ID: usize = 40;

/// The overlay layers and a revision counter (bumped by every change).
#[derive(Debug, Default)]
pub struct LayerSet {
    layers: Vec<Layer>,
    rev: u64,
    /// When each visible layer was last shown (for `hide_after_s`).
    shown_at: HashMap<String, Instant>,
    /// Where template layers find their templates.
    lib: TemplateLib,
}

/// What loading a scene did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SceneLoad {
    /// Ids of the layers added, back to front.
    pub added: Vec<String>,
    /// `"<layer id>: <why>"` for each layer left out.
    pub skipped: Vec<String>,
}

/// `name` as an id: lower case `[a-z0-9-]`, at most [`MAX_ID`] long;
/// `None` when nothing usable is left.
pub fn sanitize_id(name: &str) -> Option<String> {
    let mut out = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(MAX_ID).collect();
    let out = out.trim_end_matches('-').to_string();
    (!out.is_empty()).then_some(out)
}

/// Media given with an add/update: a file path or base64 bytes.
enum Media {
    Path(PathBuf),
    Bytes(Vec<u8>),
}

/// Take `path` / `data_base64` out of `m`.
fn take_media(m: &mut Map<String, Value>) -> Result<Option<Media>> {
    let path = m.remove("path");
    let data = m.remove("data_base64");
    match (path, data) {
        (Some(_), Some(_)) => bail!("give either path or data_base64, not both"),
        (Some(p), None) => {
            let p = p.as_str().ok_or_else(|| anyhow!("path must be text"))?;
            ensure!(!p.trim().is_empty(), "path is empty");
            Ok(Some(Media::Path(PathBuf::from(p.trim()))))
        }
        (None, Some(d)) => {
            let d = d
                .as_str()
                .ok_or_else(|| anyhow!("data_base64 must be text"))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(d.trim())
                .map_err(|e| anyhow!("data_base64 is not valid base64: {e}"))?;
            Ok(Some(Media::Bytes(bytes)))
        }
        (None, None) => Ok(None),
    }
}

/// Store `media` for layer `id` of `kind`; the file name.
fn store_media(assets: &Assets, id: &str, kind: &str, media: Media) -> Result<String> {
    let want = match kind {
        "image" => MediaKind::Image,
        "video" => MediaKind::Video,
        _ => bail!("a {kind} layer takes no file"),
    };
    match media {
        Media::Path(p) => assets.store_path(id, &p, want),
        Media::Bytes(b) => {
            ensure!(
                want == MediaKind::Image,
                "videos are given by path (data_base64 is for pictures)"
            );
            assets.store(id, &b, want)
        }
    }
}

fn hash_of(s: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn content_hash(l: &Layer) -> String {
    hash_of(&serde_json::to_string(&l.content).unwrap_or_default())
}

/// Merge a partial `params` change into `old` (`null` removes a value, so
/// it takes the template's default again).
fn merge_params(old: &Map<String, Value>, patch: &Value) -> Result<Map<String, Value>> {
    let p = patch
        .as_object()
        .ok_or_else(|| anyhow!("params must be an object of parameter values"))?;
    let mut m = old.clone();
    for (k, v) in p {
        if v.is_null() {
            m.remove(k);
        } else {
            m.insert(k.clone(), v.clone());
        }
    }
    Ok(m)
}

impl LayerSet {
    pub fn new() -> LayerSet {
        LayerSet::default()
    }

    /// Use `lib` for template layers (built-ins only until this is called).
    pub fn set_templates(&mut self, lib: TemplateLib) {
        self.lib = lib;
        self.touch();
    }

    /// The template library template layers use.
    pub fn templates(&self) -> &TemplateLib {
        &self.lib
    }

    fn template(&self, id: &str) -> Result<Template> {
        self.lib
            .get(id)
            .ok_or_else(|| anyhow!("unknown template {id:?} (list_templates shows the ids)"))
    }

    /// For a template layer `m` (layer fields): check `params` against the
    /// template and store the checked values.
    fn check_template_fields(&self, m: &mut Map<String, Value>) -> Result<Template> {
        let id = m
            .get("template")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("a template layer needs template (an id from list_templates)"))?
            .to_string();
        let t = self.template(&id)?;
        let given = match m.get("params") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(p)) => p.clone(),
            Some(_) => bail!("params must be an object of parameter values"),
        };
        let checked = t.check_params(&given)?;
        m.insert("params".into(), Value::Object(checked));
        Ok(t)
    }

    /// Layers as loaded at start-up: all hidden (nothing appears in a call
    /// by surprise after a restart).
    pub fn from_layers(mut layers: Vec<Layer>) -> LayerSet {
        let mut seen = Vec::new();
        layers.retain(|l| {
            let fresh = !seen.contains(&l.id);
            seen.push(l.id.clone());
            fresh
        });
        layers.truncate(MAX_LAYERS);
        for l in &mut layers {
            l.visible = false;
        }
        LayerSet {
            layers,
            rev: 1,
            shown_at: HashMap::new(),
            lib: TemplateLib::default(),
        }
    }

    /// Read `layers.json` (a missing file is an empty set). Layers that no
    /// longer validate are dropped (and the error is returned with them).
    pub fn load(path: &Path) -> Result<(LayerSet, Vec<String>)> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((LayerSet::new(), Vec::new()))
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let v: Value =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let items = v
            .get("layers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let (mut ok, mut bad) = (Vec::new(), Vec::new());
        for item in items {
            match from_json(item) {
                Ok(l) => ok.push(l),
                Err(e) => bad.push(format!("{e:#}")),
            }
        }
        Ok((LayerSet::from_layers(ok), bad))
    }

    /// Write `layers.json` (atomically: temp file, then rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let v = json!({ "layers": self.layers.iter().map(Layer::to_json).collect::<Vec<_>>() });
        let tmp = path.with_extension("json.part");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&v)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// The layers, back to front (by `z`, then in the order added).
    pub fn layers(&self) -> Vec<&Layer> {
        let mut v: Vec<&Layer> = self.layers.iter().collect();
        v.sort_by_key(|l| l.z);
        v
    }

    pub fn get(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    /// Bumped by every change (the scene page reloads its state then).
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Whether any layer is visible (the overlay pass runs only then).
    pub fn any_visible(&self) -> bool {
        self.layers.iter().any(|l| l.visible)
    }

    /// The stored files every layer uses.
    pub fn assets_in_use(&self) -> Vec<&str> {
        self.layers
            .iter()
            .filter_map(|l| l.content.asset())
            .collect()
    }

    fn touch(&mut self) {
        self.rev += 1;
    }

    fn mark_shown(&mut self, id: &str, now: Instant) {
        self.shown_at.insert(id.to_string(), now);
    }

    fn index(&self, id: &str) -> Result<usize> {
        self.layers
            .iter()
            .position(|l| l.id == id)
            .ok_or_else(|| anyhow!("no layer {id:?} (list_layers shows the ids)"))
    }

    /// A free id for a new layer: `wanted` (sanitised; an error if taken),
    /// else from `name`, else `<kind>-<n>`.
    fn new_id(&self, wanted: Option<&str>, name: &str, kind: &str) -> Result<String> {
        let taken = |id: &str| self.layers.iter().any(|l| l.id == id);
        if let Some(w) = wanted {
            let id = sanitize_id(w).ok_or_else(|| anyhow!("id {w:?} has no usable characters"))?;
            ensure!(
                !taken(&id),
                "a layer {id:?} exists already (change it with update_layer, or remove it first)"
            );
            return Ok(id);
        }
        let base = sanitize_id(name).unwrap_or_else(|| kind.to_string());
        let base: String = base.chars().take(MAX_ID - 4).collect();
        if !taken(&base) && sanitize_id(name).is_some() {
            return Ok(base);
        }
        (1..1000)
            .map(|n| format!("{base}-{n}"))
            .find(|id| !taken(id))
            .ok_or_else(|| anyhow!("no free id"))
    }

    /// Add a layer from a JSON object: `kind` plus any layer fields, and for
    /// media `path` (a file to copy) or `data_base64` (a picture). Missing
    /// geometry gets the kind's default; it is shown unless `visible` is
    /// false, in front of the others unless `z` is given.
    pub fn add(&mut self, spec: Value, assets: &Assets, now: Instant) -> Result<Layer> {
        let mut m = match spec {
            Value::Object(m) => m,
            _ => bail!("a layer is a JSON object"),
        };
        ensure!(
            self.layers.len() < MAX_LAYERS,
            "at most {MAX_LAYERS} layers (remove some first)"
        );
        let kind = m
            .remove("kind")
            .and_then(|k| k.as_str().map(str::to_string))
            .ok_or_else(|| anyhow!("kind is required ({})", KINDS.join(", ")))?;
        ensure!(
            KINDS.contains(&kind.as_str()),
            "unknown kind {kind:?} (use one of {})",
            KINDS.join(", ")
        );
        let media = take_media(&mut m)?;
        let wanted = match m.remove("id") {
            Some(Value::String(s)) => Some(s),
            Some(Value::Null) | None => None,
            Some(_) => bail!("id must be text"),
        };
        let name = m
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let tpl = if kind == "template" {
            let t = m.get("template").and_then(Value::as_str).ok_or_else(|| {
                anyhow!("a template layer needs template (an id from list_templates)")
            })?;
            Some(self.template(t)?)
        } else {
            None
        };
        // A template layer without a name is named after its template.
        let hint = match (&tpl, name.trim().is_empty()) {
            (Some(t), true) => t.id.clone(),
            _ => name.clone(),
        };
        let id = self.new_id(wanted.as_deref(), &hint, &kind)?;
        let (x, y, w, h) = tpl
            .as_ref()
            .map_or_else(|| default_rect(&kind), Template::geometry);
        let z = self.layers.iter().map(|l| l.z).max().map_or(0, |z| z + 1);
        let mut base = json!({
            "id": id, "name": name, "kind": kind,
            "x": x, "y": y, "w": w, "h": h, "z": z,
            "visible": true, "opacity": 1.0, "enter": "fade", "exit": "fade",
        })
        .as_object()
        .cloned()
        .expect("object literal");
        // Required content fields start empty so the error names them.
        match kind.as_str() {
            "title" => {
                base.insert("text".into(), json!(""));
            }
            "ticker" => {
                base.insert("items".into(), json!([]));
            }
            "html" => {
                base.insert("html".into(), json!(""));
            }
            "youtube" if !m.contains_key("link") => bail!("a youtube layer needs a link"),
            "youtube" => {
                base.insert("video".into(), json!(""));
            }
            "image" if media.is_none() => {
                bail!("an image layer needs path (a file) or data_base64 (the picture)")
            }
            "video" if media.is_none() && !m.contains_key("url") => {
                bail!("a video layer needs path (an MP4/WebM file) or url")
            }
            _ => {}
        }
        merge(&mut base, &m)?;
        if tpl.is_some() {
            self.check_template_fields(&mut base)?;
        }
        // Validate everything else before copying any file.
        if let Some(Media::Path(_) | Media::Bytes(_)) = &media {
            let mut probe = base.clone();
            probe.insert("asset".into(), json!("probe.png"));
            from_json(Value::Object(probe))?;
        }
        if let Some(media) = media {
            let name = store_media(assets, &id, &kind, media)?;
            base.insert("asset".into(), json!(name));
        }
        let layer = match from_json(Value::Object(base)) {
            Ok(l) => l,
            Err(e) => {
                bail!("{e:#}")
            }
        };
        if layer.visible {
            self.mark_shown(&layer.id, now);
        }
        self.layers.push(layer.clone());
        self.touch();
        Ok(layer)
    }

    /// Change some fields of a layer (any layer field, plus `path` /
    /// `data_base64` for a new picture or video file). Errors change nothing.
    pub fn update(
        &mut self,
        id: &str,
        patch: Value,
        assets: &Assets,
        now: Instant,
    ) -> Result<Layer> {
        let i = self.index(id)?;
        let mut m = match patch {
            Value::Object(m) => m,
            _ => bail!("changes are a JSON object"),
        };
        let media = take_media(&mut m)?;
        let old = self.layers[i].clone();
        let mut base = old
            .to_json()
            .as_object()
            .cloned()
            .expect("layer is an object");
        if let Content::Template {
            template, params, ..
        } = &old.content
        {
            // A new template starts from its own defaults; params merge.
            let changed = m
                .get("template")
                .is_some_and(|t| t.as_str() != Some(template.as_str()));
            let start = if changed { Map::new() } else { params.clone() };
            if let Some(p) = m.get("params") {
                let merged = merge_params(&start, p)?;
                m.insert("params".into(), Value::Object(merged));
            } else if changed {
                m.insert("params".into(), Value::Object(Map::new()));
            }
        }
        merge(&mut base, &m)?;
        if old.content.kind() == "template"
            && (m.contains_key("params") || m.contains_key("template"))
        {
            self.check_template_fields(&mut base)?;
        }
        // Check the rest first, then store the file.
        let mut layer = from_json(Value::Object(base.clone()))?;
        if let Some(media) = media {
            let name = store_media(assets, id, old.content.kind(), media)?;
            base.insert("asset".into(), json!(name));
            layer = match from_json(Value::Object(base)) {
                Ok(l) => l,
                Err(e) => {
                    assets.remove(&name);
                    return Err(e);
                }
            };
        }
        if let Some(a) = old.content.asset() {
            if layer.content.asset() != Some(a) {
                assets.remove(a);
            }
        }
        if layer.visible && !old.visible {
            self.mark_shown(id, now);
        }
        // A new auto-hide time counts from now.
        if layer.visible && m.contains_key("hide_after_s") {
            self.mark_shown(id, now);
        }
        self.layers[i] = layer.clone();
        self.touch();
        Ok(layer)
    }

    /// A new file for an image or video layer (an upload from the control
    /// page; recognised by its first bytes).
    pub fn set_asset(&mut self, id: &str, bytes: &[u8], assets: &Assets) -> Result<Layer> {
        let i = self.index(id)?;
        let old = self.layers[i].clone();
        let want = match old.content.kind() {
            "image" => MediaKind::Image,
            "video" => MediaKind::Video,
            k => bail!("a {k} layer takes no file"),
        };
        let name = assets.store(id, bytes, want)?;
        let mut base = old
            .to_json()
            .as_object()
            .cloned()
            .expect("layer is an object");
        base.insert("asset".into(), json!(name));
        let layer = match from_json(Value::Object(base)) {
            Ok(l) => l,
            Err(e) => {
                assets.remove(&name);
                return Err(e);
            }
        };
        if let Some(a) = old.content.asset() {
            assets.remove(a);
        }
        self.layers[i] = layer.clone();
        self.touch();
        Ok(layer)
    }

    /// Show a layer (plays its enter animation; restarts its auto-hide).
    pub fn show(&mut self, id: &str, now: Instant) -> Result<Layer> {
        let i = self.index(id)?;
        self.layers[i].visible = true;
        self.mark_shown(id, now);
        self.touch();
        Ok(self.layers[i].clone())
    }

    /// Hide a layer (plays its exit animation).
    pub fn hide(&mut self, id: &str) -> Result<Layer> {
        let i = self.index(id)?;
        self.layers[i].visible = false;
        self.touch();
        Ok(self.layers[i].clone())
    }

    /// Delete a layer and its stored file.
    pub fn remove(&mut self, id: &str, assets: &Assets) -> Result<Layer> {
        let i = self.index(id)?;
        let l = self.layers.remove(i);
        if let Some(a) = l.content.asset() {
            assets.remove(a);
        }
        self.shown_at.remove(id);
        self.touch();
        Ok(l)
    }

    /// Hide every layer; how many were visible.
    pub fn clear(&mut self) -> usize {
        let n = self.layers.iter().filter(|l| l.visible).count();
        for l in &mut self.layers {
            l.visible = false;
        }
        self.touch();
        n
    }

    /// Restack: `ids` from back to front; layers not listed keep their
    /// order behind them.
    pub fn reorder(&mut self, ids: &[String]) -> Result<()> {
        for id in ids {
            self.index(id)?;
        }
        let mut seen: Vec<&String> = Vec::new();
        for id in ids {
            ensure!(!seen.contains(&id), "{id:?} is listed twice");
            seen.push(id);
        }
        let mut order: Vec<String> = self
            .layers()
            .into_iter()
            .filter(|l| !ids.contains(&l.id))
            .map(|l| l.id.clone())
            .collect();
        order.extend(ids.iter().cloned());
        for (z, id) in order.iter().enumerate() {
            let i = self.index(id)?;
            self.layers[i].z = z as i32;
        }
        self.touch();
        Ok(())
    }

    /// Hide layers whose `hide_after_s` has passed; true if any was hidden.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for l in &mut self.layers {
            let (Some(s), true) = (l.hide_after_s, l.visible) else {
                continue;
            };
            let due = self
                .shown_at
                .get(&l.id)
                .is_none_or(|t| now.saturating_duration_since(*t) >= Duration::from_secs(s.into()));
            if due {
                l.visible = false;
                changed = true;
            }
        }
        if changed {
            self.touch();
        }
        changed
    }

    /// What the scene page draws: the revision and the layers back to
    /// front, each with a content version `v` (HTML fragments are fetched
    /// from their own URL, so they are left out here).
    pub fn state_json(&self) -> Value {
        let layers: Vec<Value> = self
            .layers()
            .into_iter()
            .map(|l| {
                let mut v = l.to_json();
                if let Some(o) = v.as_object_mut() {
                    o.remove("html");
                    match &l.content {
                        Content::Template { template, params } => {
                            // `v` reloads the frame (another template, or its
                            // HTML changed); `pv` only posts the params.
                            let t = self.lib.get(template);
                            let eff = t
                                .as_ref()
                                .map_or_else(|| params.clone(), |t| t.effective(params));
                            let html = t.as_ref().map_or("", |t| t.html.as_str());
                            let missing = if t.is_none() { "missing" } else { "" };
                            o.insert(
                                "v".into(),
                                json!(hash_of(&format!("template\0{template}\0{missing}\0{html}"))),
                            );
                            let eff = Value::Object(eff);
                            o.insert("pv".into(), json!(hash_of(&eff.to_string())));
                            o.insert("params".into(), eff);
                        }
                        _ => {
                            o.insert("v".into(), json!(content_hash(l)));
                        }
                    }
                }
                v
            })
            .collect();
        json!({ "rev": self.rev, "layers": layers })
    }

    /// A layer as callers see it: its fields, and `template_missing: true`
    /// for a template layer whose template is gone.
    pub fn layer_json(&self, l: &Layer) -> Value {
        let mut v = l.to_json();
        if let Content::Template { template, .. } = &l.content {
            if self.lib.get(template).is_none() {
                v["template_missing"] = json!(true);
            }
        }
        v
    }

    /// Every layer as [`LayerSet::layer_json`], back to front.
    pub fn layers_json(&self) -> Vec<Value> {
        self.layers()
            .into_iter()
            .map(|l| self.layer_json(l))
            .collect()
    }

    /// The HTML fragment of an `html` layer.
    pub fn html_of(&self, id: &str) -> Option<&str> {
        match &self.get(id)?.content {
            Content::Html { html } => Some(html),
            _ => None,
        }
    }

    /// The document an `html` or `template` layer's frame loads.
    pub fn layer_page(&self, id: &str) -> Option<String> {
        match &self.get(id)?.content {
            Content::Html { html } => Some(crate::scene::layer_page(html)),
            Content::Template { template, params } => Some(match self.lib.get(template) {
                Some(t) => crate::scene::template_page(&t, &t.effective(params)),
                None => crate::scene::missing_page(template),
            }),
            _ => None,
        }
    }

    /// Every layer as a scene keeps it (visible or not), back to front.
    pub fn scene_layers(&self) -> Vec<Value> {
        self.layers().into_iter().map(Layer::to_json).collect()
    }

    /// Bring back a scene's layers (shown as saved). `replace` removes every
    /// layer first; otherwise they are added in front, with new ids where
    /// theirs are taken. Layers that can't come back (template or file
    /// gone, too many layers) are skipped and named.
    pub fn load_scene(
        &mut self,
        scene: &crate::scenes::Scene,
        replace: bool,
        assets: &Assets,
        now: Instant,
    ) -> Result<SceneLoad> {
        if replace {
            let ids: Vec<String> = self.layers.iter().map(|l| l.id.clone()).collect();
            for id in ids {
                self.remove(&id, assets)?;
            }
        }
        let mut out = SceneLoad::default();
        for l in &scene.layers {
            let Some(mut m) = l.as_object().cloned() else {
                continue;
            };
            let id = m
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            if let Some(t) = m.get("template").and_then(Value::as_str) {
                if self.lib.get(t).is_none() {
                    out.skipped
                        .push(format!("{id}: template {t:?} no longer exists"));
                    continue;
                }
            }
            if let Some(a) = m
                .remove("asset")
                .and_then(|a| a.as_str().map(str::to_string))
            {
                match assets.path_of(&a).filter(|p| p.is_file()) {
                    Some(p) => {
                        m.insert("path".into(), json!(p.to_string_lossy()));
                    }
                    None => {
                        out.skipped
                            .push(format!("{id}: its picture or video file is gone"));
                        continue;
                    }
                }
            }
            if self.get(&id).is_some() {
                m.remove("id");
            }
            if !replace {
                m.remove("z");
            }
            match self.add(Value::Object(m), assets, now) {
                Ok(l) => out.added.push(l.id),
                Err(e) => out.skipped.push(format!("{id}: {e:#}")),
            }
        }
        Ok(out)
    }

    /// Save a user template from a request: a full definition, or
    /// `{from_layer: <html layer id>, id?, name?, …}`, or
    /// `{copy_of: <template id>, id, name?, …fields to change}`.
    pub fn save_template(&mut self, req: Value) -> Result<Template> {
        let mut m = match req {
            Value::Object(m) => m,
            _ => bail!("a template is a JSON object"),
        };
        if let Some(layer) = m.remove("from_layer") {
            let lid = layer
                .as_str()
                .ok_or_else(|| anyhow!("from_layer must be a layer id"))?;
            let l = self
                .get(lid)
                .ok_or_else(|| anyhow!("no layer {lid:?} (list_layers shows the ids)"))?;
            let html = match &l.content {
                Content::Html { html } => html.clone(),
                _ => bail!(
                    "from_layer must be an html layer ({lid:?} is a {} layer)",
                    l.content.kind()
                ),
            };
            let name = m
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    if l.name.is_empty() {
                        l.id.clone()
                    } else {
                        l.name.clone()
                    }
                });
            if !m.contains_key("id") {
                let id = sanitize_id(&name).unwrap_or_else(|| "my-template".into());
                m.insert("id".into(), json!(id));
            }
            m.insert("name".into(), json!(name));
            m.insert("html".into(), json!(html));
            m.entry("rect").or_insert(json!([l.x, l.y, l.w, l.h]));
            m.entry("category").or_insert(json!("mine"));
        } else if let Some(src) = m.remove("copy_of") {
            let sid = src
                .as_str()
                .ok_or_else(|| anyhow!("copy_of must be a template id"))?;
            let t = self.template(sid)?;
            ensure!(m.contains_key("id"), "give the copy an id (a-z, 0-9 and -)");
            let mut base = match serde_json::to_value(&t)? {
                Value::Object(b) => b,
                _ => unreachable!("a template serializes to an object"),
            };
            base.insert("name".into(), json!(format!("{} (copy)", t.name)));
            base.insert("category".into(), json!("mine"));
            for (k, v) in m {
                base.insert(k, v);
            }
            m = base;
        }
        let t = self.lib.save(Value::Object(m))?;
        // Layers using it reload (their `v` covers the template HTML).
        self.touch();
        Ok(t)
    }

    /// Delete a user template; layers using it show "template missing".
    pub fn delete_template(&mut self, id: &str) -> Result<()> {
        self.lib.delete(id)?;
        self.touch();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::Content;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n0000";

    fn setup() -> (tempfile::TempDir, Assets, LayerSet, Instant) {
        let d = tempfile::tempdir().unwrap();
        let a = Assets::new(d.path().join("assets"));
        (d, a, LayerSet::new(), Instant::now())
    }

    #[test]
    fn ids_are_sanitised_and_unique() {
        assert_eq!(
            sanitize_id(" Lower Third #1 ").as_deref(),
            Some("lower-third-1")
        );
        assert_eq!(sanitize_id("!!!"), None);
        assert_eq!(sanitize_id(&"a".repeat(99)).unwrap().len(), MAX_ID);
        let (_d, a, mut s, now) = setup();
        let t = s
            .add(json!({"kind": "title", "text": "Hi"}), &a, now)
            .unwrap();
        assert_eq!(t.id, "title-1");
        let t2 = s
            .add(json!({"kind": "title", "text": "Hi"}), &a, now)
            .unwrap();
        assert_eq!(t2.id, "title-2");
        let n = s
            .add(
                json!({"kind": "title", "text": "x", "name": "Guest Name"}),
                &a,
                now,
            )
            .unwrap();
        assert_eq!(n.id, "guest-name");
        let n2 = s
            .add(
                json!({"kind": "title", "text": "x", "name": "Guest Name"}),
                &a,
                now,
            )
            .unwrap();
        assert_eq!(n2.id, "guest-name-1");
        let e = s
            .add(
                json!({"kind": "title", "text": "x", "id": "Guest-Name"}),
                &a,
                now,
            )
            .unwrap_err();
        assert!(e.to_string().contains("exists already"), "{e}");
    }

    #[test]
    fn add_fills_defaults_and_stacks_in_front() {
        let (_d, a, mut s, now) = setup();
        let t = s
            .add(json!({"kind": "ticker", "items": ["a", "b"]}), &a, now)
            .unwrap();
        assert_eq!((t.x, t.y, t.w, t.h, t.z), (0.0, 0.92, 1.0, 0.08, 0));
        assert!(t.visible);
        let h = s
            .add(
                json!({"kind": "html", "html": "<p>x</p>", "visible": false}),
                &a,
                now,
            )
            .unwrap();
        assert_eq!(h.z, 1);
        assert!(!h.visible);
        assert!(s.any_visible());
        for (spec, msg) in [
            (json!({"text": "x"}), "kind is required"),
            (json!({"kind": "clock"}), "unknown kind"),
            (json!({"kind": "title"}), "needs text"),
            (json!({"kind": "ticker"}), "at least one item"),
            (json!({"kind": "image"}), "needs path"),
            (json!({"kind": "video"}), "needs path"),
            (json!({"kind": "youtube"}), "needs a link"),
            (
                json!({"kind": "title", "text": "x", "html": "y"}),
                "unknown field",
            ),
            (json!(["x"]), "JSON object"),
        ] {
            let e = s.add(spec.clone(), &a, now).unwrap_err().to_string();
            assert!(e.contains(msg), "{spec}: {e}");
        }
        assert_eq!(s.layers().len(), 2, "failed adds change nothing");
    }

    #[test]
    fn media_is_copied_and_cleaned_up() {
        let (d, a, mut s, now) = setup();
        let b64 = base64::engine::general_purpose::STANDARD.encode(PNG);
        let img = s
            .add(json!({"kind": "image", "data_base64": b64}), &a, now)
            .unwrap();
        let first = img.content.asset().unwrap().to_string();
        assert!(a.read(&first).is_some());
        let p = d.path().join("logo.png");
        std::fs::write(&p, PNG).unwrap();
        let img = s
            .update(&img.id, json!({"path": p.to_string_lossy()}), &a, now)
            .unwrap();
        let second = img.content.asset().unwrap().to_string();
        assert_ne!(first, second);
        assert!(a.read(&first).is_none(), "replaced file deleted");
        // A failed change leaves the layer and its file alone.
        assert!(s
            .update(
                &img.id,
                json!({"path": d.path().join("none.png").to_string_lossy()}),
                &a,
                now
            )
            .is_err());
        assert!(s.update(&img.id, json!({"x": "a"}), &a, now).is_err());
        assert_eq!(
            s.get(&img.id).unwrap().content.asset(),
            Some(second.as_str())
        );
        assert!(a.read(&second).is_some());
        let e = s
            .add(json!({"kind": "video", "data_base64": "AAAA"}), &a, now)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("not a supported file") || e.contains("by path"),
            "{e}"
        );
        s.remove(&img.id, &a).unwrap();
        assert!(a.read(&second).is_none());
    }

    #[test]
    fn show_hide_clear_reorder() {
        let (_d, a, mut s, now) = setup();
        for i in 0..3 {
            s.add(
                json!({"kind": "title", "text": "t", "id": format!("l{i}")}),
                &a,
                now,
            )
            .unwrap();
        }
        let r = s.rev();
        s.hide("l0").unwrap();
        assert!(s.rev() > r);
        assert!(!s.get("l0").unwrap().visible);
        s.show("l0", now).unwrap();
        assert!(s.get("l0").unwrap().visible);
        assert_eq!(s.clear(), 3);
        assert!(!s.any_visible());
        s.reorder(&["l2".into(), "l0".into()]).unwrap();
        let order: Vec<_> = s.layers().iter().map(|l| l.id.clone()).collect();
        assert_eq!(order, ["l1", "l2", "l0"]);
        assert!(s.reorder(&["nope".into()]).is_err());
        assert!(s.reorder(&["l1".into(), "l1".into()]).is_err());
        assert!(s
            .hide("nope")
            .unwrap_err()
            .to_string()
            .contains("list_layers"));
    }

    #[test]
    fn auto_hide_counts_from_showing() {
        let (_d, a, mut s, now) = setup();
        s.add(
            json!({"kind": "title", "text": "t", "id": "t", "hide_after_s": 5}),
            &a,
            now,
        )
        .unwrap();
        assert!(!s.tick(now + Duration::from_secs(4)));
        assert!(s.tick(now + Duration::from_secs(5)));
        assert!(!s.get("t").unwrap().visible);
        let later = now + Duration::from_secs(60);
        s.show("t", later).unwrap();
        assert!(!s.tick(later + Duration::from_secs(4)));
        assert!(s.tick(later + Duration::from_secs(6)));
        assert!(!s.tick(later + Duration::from_secs(7)), "only once");
    }

    #[test]
    fn saved_layers_come_back_hidden() {
        let (d, a, mut s, now) = setup();
        s.add(json!({"kind": "title", "text": "t", "id": "t"}), &a, now)
            .unwrap();
        s.add(
            json!({"kind": "html", "html": "<i>x</i>", "id": "h"}),
            &a,
            now,
        )
        .unwrap();
        let p = d.path().join("overlays").join("layers.json");
        s.save(&p).unwrap();
        let (back, bad) = LayerSet::load(&p).unwrap();
        assert!(bad.is_empty());
        assert_eq!(back.layers().len(), 2);
        assert!(!back.any_visible());
        assert!(matches!(
            back.get("h").unwrap().content,
            Content::Html { .. }
        ));
        let (empty, _) = LayerSet::load(&d.path().join("missing.json")).unwrap();
        assert!(empty.layers().is_empty());
        std::fs::write(&p, r#"{"layers":[{"id":"x","kind":"title"}]}"#).unwrap();
        let (some, bad) = LayerSet::load(&p).unwrap();
        assert!(some.layers().is_empty());
        assert_eq!(bad.len(), 1);
    }

    #[test]
    fn state_leaves_html_out_and_versions_content() {
        let (_d, a, mut s, now) = setup();
        s.add(
            json!({"kind": "html", "html": "<i>x</i>", "id": "h"}),
            &a,
            now,
        )
        .unwrap();
        let st = s.state_json();
        let l = &st["layers"][0];
        assert!(l.get("html").is_none());
        let v1 = l["v"].as_str().unwrap().to_string();
        s.update("h", json!({"x": 0.1}), &a, now).unwrap();
        assert_eq!(
            s.state_json()["layers"][0]["v"],
            v1.as_str(),
            "moving keeps v"
        );
        s.update("h", json!({"html": "<b>y</b>"}), &a, now).unwrap();
        assert_ne!(s.state_json()["layers"][0]["v"], v1.as_str());
        assert_eq!(s.html_of("h"), Some("<b>y</b>"));
        assert_eq!(st["rev"], 1);
    }

    #[test]
    fn template_layer_add_update_partial_params() {
        let (_d, a, mut s, now) = setup();
        let l = s
            .add(
                json!({"kind": "template", "template": "lower-third",
                    "params": {"title": "Alex", "subtitle": "CTO", "theme": "news"}}),
                &a,
                now,
            )
            .unwrap();
        assert_eq!(l.id, "lower-third");
        assert_eq!(
            (l.x, l.y, l.w, l.h),
            (0.05, 0.74, 0.5, 0.15),
            "the template's rect"
        );
        let l = s
            .update(
                &l.id,
                json!({"params": {"subtitle": null, "title": "Sam"}}),
                &a,
                now,
            )
            .unwrap();
        match &l.content {
            Content::Template { params, .. } => {
                assert_eq!(params["title"], "Sam");
                assert_eq!(params["theme"], "news", "untouched params stay");
                assert!(params.get("subtitle").is_none(), "null resets to default");
            }
            c => panic!("{c:?}"),
        }
        for (spec, msg) in [
            (json!({"kind": "template"}), "needs template"),
            (
                json!({"kind": "template", "template": "nope"}),
                "unknown template",
            ),
            (
                json!({"kind": "template", "template": "lower-third"}),
                "\"title\" is required",
            ),
            (
                json!({"kind": "template", "template": "lower-third", "params": {"title": "x", "size": 3}}),
                "unknown param \"size\"",
            ),
            (
                json!({"kind": "template", "template": "lower-third", "params": [1]}),
                "object",
            ),
        ] {
            let e = s.add(spec.clone(), &a, now).unwrap_err().to_string();
            assert!(e.contains(msg), "{spec}: {e}");
        }
        let e = s
            .update("lower-third", json!({"params": {"title": ""}}), &a, now)
            .unwrap_err()
            .to_string();
        assert!(e.contains("required"), "{e}");
        // Another template: its own defaults, params checked against it.
        let l = s
            .update(
                "lower-third",
                json!({"template": "countdown", "params": {"label": "Back in"}}),
                &a,
                now,
            )
            .unwrap();
        assert!(
            matches!(&l.content, Content::Template { template, .. } if template == "countdown")
        );
        // A full-frame template covers the output.
        let c = s
            .add(
                json!({"kind": "template", "template": "starting-soon"}),
                &a,
                now,
            )
            .unwrap();
        assert_eq!((c.x, c.y, c.w, c.h), (0.0, 0.0, 1.0, 1.0));
    }

    #[test]
    fn params_change_keeps_v() {
        let (_d, a, mut s, now) = setup();
        s.add(
            json!({"kind": "template", "template": "countdown", "id": "c", "params": {"duration": 60}}),
            &a,
            now,
        )
        .unwrap();
        let st = s.state_json();
        let l = &st["layers"][0];
        assert_eq!(l["params"]["duration"], 60);
        assert_eq!(l["params"]["label"], "Starting in", "defaults filled");
        let (v, pv) = (l["v"].clone(), l["pv"].clone());
        s.update("c", json!({"params": {"duration": 30}}), &a, now)
            .unwrap();
        let st = s.state_json();
        assert_eq!(st["layers"][0]["v"], v, "same frame");
        assert_ne!(st["layers"][0]["pv"], pv, "new params");
        s.update("c", json!({"x": 0.1}), &a, now).unwrap();
        assert_eq!(s.state_json()["layers"][0]["v"], v);
    }

    #[test]
    fn layer_page_escapes_params() {
        let (_d, a, mut s, now) = setup();
        s.add(
            json!({"kind": "template", "template": "lower-third", "id": "t",
                "params": {"title": "</script><script>alert(1)</script>"}}),
            &a,
            now,
        )
        .unwrap();
        let p = s.layer_page("t").unwrap();
        assert_eq!(
            p.matches("</script>").count(),
            3,
            "runtime, params, template: {p}"
        );
        assert!(p.contains("\\u003c/script\\u003e"));
        assert!(p.contains("P2C.onParams"));
        s.add(json!({"kind": "title", "text": "x", "id": "x"}), &a, now)
            .unwrap();
        assert!(s.layer_page("x").is_none());
    }

    #[test]
    fn missing_template_is_reported() {
        let (_d, a, mut s, now) = setup();
        let lib = TemplateLib::builtin_only();
        lib.save(json!({"id": "mine", "name": "Mine", "html": "<i>x</i><script>P2C.onParams(function(){})</script>"}))
            .unwrap();
        s.set_templates(lib.clone());
        s.add(
            json!({"kind": "template", "template": "mine", "id": "m"}),
            &a,
            now,
        )
        .unwrap();
        assert!(s.layers_json()[0].get("template_missing").is_none());
        lib.delete("mine").unwrap();
        assert_eq!(s.layers_json()[0]["template_missing"], true);
        assert!(s.layer_page("m").unwrap().contains("missing"));
        let st = s.state_json();
        assert!(st["layers"][0]["v"].is_string());
        // Hiding and moving still work; changing params says why not.
        s.update("m", json!({"visible": false, "x": 0.2}), &a, now)
            .unwrap();
        let e = s
            .update("m", json!({"params": {"a": 1}}), &a, now)
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown template"), "{e}");
    }

    #[test]
    fn templates_saved_from_layers_and_copies() {
        let (_d, a, mut s, now) = setup();
        s.add(
            json!({"kind": "html", "id": "chart", "name": "Sales chart", "html": "<b>x</b>"}),
            &a,
            now,
        )
        .unwrap();
        let t = s.save_template(json!({"from_layer": "chart"})).unwrap();
        assert_eq!(
            (t.id.as_str(), t.name.as_str()),
            ("sales-chart", "Sales chart")
        );
        assert_eq!(t.html, "<b>x</b>");
        let c = s
            .save_template(json!({"copy_of": "lower-third", "id": "my-lt", "description": "mine"}))
            .unwrap();
        assert_eq!(c.html, crate::builtin("lower-third").unwrap().html);
        assert_eq!(c.category, "mine");
        assert!(!c.builtin);
        assert!(s.save_template(json!({"copy_of": "lower-third"})).is_err());
        assert!(s.save_template(json!({"from_layer": "nope"})).is_err());
        s.add(
            json!({"kind": "template", "template": "my-lt", "params": {"title": "x"}}),
            &a,
            now,
        )
        .unwrap();
    }

    #[test]
    fn layer_count_is_limited() {
        let (_d, a, mut s, now) = setup();
        for _ in 0..MAX_LAYERS {
            s.add(json!({"kind": "title", "text": "t"}), &a, now)
                .unwrap();
        }
        let e = s.add(json!({"kind": "title", "text": "t"}), &a, now);
        assert!(e.unwrap_err().to_string().contains("at most"));
    }
}
