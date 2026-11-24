//! Saved looks ("scenes"): the overlay layers (and optionally the
//! background and webcam choices) under a name, recalled in one call.
//! Stored in `<data dir>/overlays/scenes/<id>.json`; pictures and videos
//! the layers use are copied, so a scene keeps working after the layers
//! that made it are removed.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::assets::Assets;
use crate::set::{sanitize_id, MAX_LAYERS};

/// Most scenes.
pub const MAX_SCENES: usize = 100;

/// One saved look.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub id: String,
    pub name: String,
    /// Layer definitions, back to front (as `list_layers` shows them).
    pub layers: Vec<Value>,
    /// The background choices (`BackgroundDto`), if saved with them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Value>,
    /// The webcam overlay (`{visible, style, x, y, width}`), if saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webcam: Option<Value>,
    /// Seconds since 1970.
    #[serde(default)]
    pub created: u64,
}

impl Scene {
    /// The stored files its layers use.
    pub fn assets(&self) -> Vec<String> {
        self.layers
            .iter()
            .filter_map(|l| l["asset"].as_str().map(str::to_string))
            .collect()
    }

    /// A short description for lists.
    pub fn summary(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "layers": self.layers.len(),
            "visible_layers": self.layers.iter().filter(|l| l["visible"] == true).count(),
            "has_background": self.background.is_some(),
            "has_webcam": self.webcam.is_some(),
            "created": self.created,
        })
    }
}

/// The saved scenes.
#[derive(Debug, Default)]
pub struct SceneStore {
    dir: Option<PathBuf>,
    scenes: BTreeMap<String, Scene>,
}

impl SceneStore {
    /// Scenes kept in memory only.
    pub fn in_memory() -> SceneStore {
        SceneStore::default()
    }

    /// The scenes saved in `dir`; unreadable files are skipped and named.
    pub fn open(dir: PathBuf) -> (SceneStore, Vec<String>) {
        let mut scenes = BTreeMap::new();
        let mut bad = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_none_or(|x| x != "json") {
                    continue;
                }
                let r = std::fs::read_to_string(&p)
                    .map_err(anyhow::Error::from)
                    .and_then(|t| serde_json::from_str::<Scene>(&t).map_err(Into::into));
                match r {
                    Ok(s) if sanitize_id(&s.id).as_deref() == Some(s.id.as_str()) => {
                        scenes.insert(s.id.clone(), s);
                    }
                    Ok(_) => bad.push(format!("{}: bad id", p.display())),
                    Err(e) => bad.push(format!("{}: {e:#}", p.display())),
                }
            }
        }
        (
            SceneStore {
                dir: Some(dir),
                scenes,
            },
            bad,
        )
    }

    /// Summaries, by name.
    pub fn list(&self) -> Vec<Value> {
        let mut v: Vec<&Scene> = self.scenes.values().collect();
        v.sort_by_key(|s| s.name.to_lowercase());
        v.into_iter().map(Scene::summary).collect()
    }

    pub fn get(&self, id: &str) -> Option<&Scene> {
        self.scenes.get(id)
    }

    /// The stored files every scene uses (kept when unused files are pruned).
    pub fn assets_in_use(&self) -> Vec<String> {
        self.scenes.values().flat_map(Scene::assets).collect()
    }

    /// Save `layers` (and optionally background/webcam) under `name`
    /// (replacing a scene with the same id). Layer files are copied.
    pub fn save(
        &mut self,
        name: &str,
        layers: Vec<Value>,
        background: Option<Value>,
        webcam: Option<Value>,
        assets: &Assets,
    ) -> Result<Scene> {
        let name = name.trim();
        ensure!(
            !name.is_empty() && name.chars().count() <= 80,
            "a scene needs a name of 1 to 80 characters"
        );
        let id = sanitize_id(name)
            .ok_or_else(|| anyhow!("the name {name:?} has no usable characters"))?;
        ensure!(
            self.scenes.contains_key(&id) || self.scenes.len() < MAX_SCENES,
            "at most {MAX_SCENES} scenes (delete some first)"
        );
        ensure!(layers.len() <= MAX_LAYERS, "at most {MAX_LAYERS} layers");
        let owner = format!("scene-{}", id.chars().take(30).collect::<String>());
        let mut copied = Vec::new();
        let mut out = Vec::new();
        for mut l in layers {
            if let Some(a) = l["asset"].as_str().map(str::to_string) {
                match assets.copy(&a, &owner) {
                    Ok(n) => {
                        copied.push(n.clone());
                        l["asset"] = json!(n);
                    }
                    Err(e) => {
                        for c in &copied {
                            assets.remove(c);
                        }
                        return Err(e);
                    }
                }
            }
            if let Some(m) = l.as_object_mut() {
                m.remove("template_missing");
                m.remove("html_bytes");
            }
            out.push(l);
        }
        let scene = Scene {
            id: id.clone(),
            name: name.to_string(),
            layers: out,
            background,
            webcam,
            created: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        if let Some(dir) = &self.dir {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            let p = dir.join(format!("{id}.json"));
            let tmp = p.with_extension("json.part");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&scene)?)
                .with_context(|| format!("writing {}", tmp.display()))?;
            std::fs::rename(&tmp, &p).with_context(|| format!("writing {}", p.display()))?;
        }
        if let Some(old) = self.scenes.insert(id, scene.clone()) {
            for a in old.assets() {
                assets.remove(&a);
            }
        }
        Ok(scene)
    }

    /// Delete a scene and its copied files.
    pub fn delete(&mut self, id: &str, assets: &Assets) -> Result<()> {
        let Some(s) = self.scenes.remove(id) else {
            bail!("no scene {id:?} (list_scenes shows the ids)");
        };
        for a in s.assets() {
            assets.remove(&a);
        }
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::set::LayerSet;
    use std::time::Instant;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n0000";

    fn setup() -> (tempfile::TempDir, Assets, LayerSet, SceneStore) {
        let d = tempfile::tempdir().unwrap();
        let a = Assets::new(d.path().join("assets"));
        let (s, bad) = SceneStore::open(d.path().join("scenes"));
        assert!(bad.is_empty());
        (d, a, LayerSet::new(), s)
    }

    fn png_b64() -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(PNG)
    }

    #[test]
    fn scene_save_load_replace_and_add() {
        let (d, a, mut set, mut store) = setup();
        let now = Instant::now();
        set.add(json!({"kind": "title", "text": "Hi", "id": "t"}), &a, now)
            .unwrap();
        set.add(
            json!({"kind": "template", "template": "countdown", "id": "c", "visible": false,
                "params": {"duration": 60}}),
            &a,
            now,
        )
        .unwrap();
        let sc = store
            .save(
                "Intro",
                set.scene_layers(),
                Some(json!({"full": "page"})),
                None,
                &a,
            )
            .unwrap();
        assert_eq!(sc.id, "intro");
        assert_eq!(store.list()[0]["layers"], 2);
        assert_eq!(store.list()[0]["visible_layers"], 1);
        // Replace: only the scene's layers, as saved.
        set.add(
            json!({"kind": "title", "text": "Other", "id": "o"}),
            &a,
            now,
        )
        .unwrap();
        let r = set.load_scene(&sc, true, &a, now).unwrap();
        assert_eq!(r.added, ["t", "c"]);
        assert!(r.skipped.is_empty());
        let ids: Vec<_> = set.layers().iter().map(|l| l.id.clone()).collect();
        assert_eq!(ids, ["t", "c"]);
        assert!(set.get("t").unwrap().visible && !set.get("c").unwrap().visible);
        // Add: new ids where taken, in front of the others.
        let r = set.load_scene(&sc, false, &a, now).unwrap();
        assert_eq!(r.added.len(), 2);
        assert_eq!(set.layers().len(), 4);
        assert_ne!(r.added[0], "t");
        assert_eq!(set.layers().last().unwrap().id, r.added[1]);
        // Saved on disk and read back.
        let (again, bad) = SceneStore::open(d.path().join("scenes"));
        assert!(bad.is_empty());
        assert_eq!(
            again.get("intro").unwrap().background,
            Some(json!({"full": "page"}))
        );
        store.delete("intro", &a).unwrap();
        assert!(store.get("intro").is_none());
        assert!(store.delete("intro", &a).is_err());
    }

    #[test]
    fn scene_assets_are_owned_copies() {
        let (_d, a, mut set, mut store) = setup();
        let now = Instant::now();
        let img = set
            .add(
                json!({"kind": "image", "id": "logo", "data_base64": png_b64()}),
                &a,
                now,
            )
            .unwrap();
        let orig = img.content.asset().unwrap().to_string();
        let sc = store
            .save("Logo", set.scene_layers(), None, None, &a)
            .unwrap();
        let copy = sc.assets()[0].clone();
        assert_ne!(copy, orig);
        assert!(copy.starts_with("scene-logo-"));
        // Removing the layer deletes its own file, not the scene's.
        set.remove("logo", &a).unwrap();
        assert!(a.read(&orig).is_none() && a.read(&copy).is_some());
        let r = set.load_scene(&sc, true, &a, now).unwrap();
        assert_eq!(r.added, ["logo"]);
        let layer_file = set
            .get("logo")
            .unwrap()
            .content
            .asset()
            .unwrap()
            .to_string();
        assert_ne!(layer_file, copy, "the layer gets its own copy");
        assert_eq!(store.assets_in_use(), [copy.clone()]);
        store.delete("logo", &a).unwrap();
        assert!(a.read(&copy).is_none() && a.read(&layer_file).is_some());
    }

    #[test]
    fn scene_load_skips_missing() {
        let (_d, a, mut set, mut store) = setup();
        let now = Instant::now();
        let lib = crate::store::TemplateLib::builtin_only();
        lib.save(json!({"id": "mine", "name": "Mine", "html": "<i>x</i><script>P2C.onParams(function(){})</script>"}))
            .unwrap();
        set.set_templates(lib.clone());
        set.add(
            json!({"kind": "template", "template": "mine", "id": "m"}),
            &a,
            now,
        )
        .unwrap();
        set.add(
            json!({"kind": "image", "id": "pic", "data_base64": png_b64()}),
            &a,
            now,
        )
        .unwrap();
        set.add(json!({"kind": "title", "text": "ok", "id": "t"}), &a, now)
            .unwrap();
        let sc = store
            .save("Show", set.scene_layers(), None, None, &a)
            .unwrap();
        lib.delete("mine").unwrap();
        a.remove(&sc.assets()[0]);
        let r = set.load_scene(&sc, true, &a, now).unwrap();
        assert_eq!(r.added, ["t"]);
        assert_eq!(r.skipped.len(), 2, "{:?}", r.skipped);
        assert!(r
            .skipped
            .iter()
            .any(|s| s.contains("m:") && s.contains("template")));
        assert!(r.skipped.iter().any(|s| s.starts_with("pic:")));
    }

    #[test]
    fn scene_limit_and_names() {
        let (_d, a, _set, mut store) = setup();
        assert!(store.save("  ", vec![], None, None, &a).is_err());
        assert!(store.save("!!!", vec![], None, None, &a).is_err());
        for n in 0..MAX_SCENES {
            store
                .save(&format!("s{n}"), vec![], None, None, &a)
                .unwrap();
        }
        assert!(store
            .save("more", vec![], None, None, &a)
            .unwrap_err()
            .to_string()
            .contains("at most"));
        store.save("s1", vec![], None, None, &a).unwrap();
    }
}
