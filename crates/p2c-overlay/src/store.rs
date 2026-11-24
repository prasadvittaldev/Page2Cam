//! The template library: the built-ins plus the user's own templates
//! (`<data dir>/overlays/templates/<id>.json`, same shape as built-ins with
//! `builtin: false`). Imported and agent-written templates are untrusted
//! code: they run in the same sandboxed frame as `html` layers.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde_json::{json, Value};

use crate::template::{builtin, builtins, Template};

/// Most user templates.
pub const MAX_USER_TEMPLATES: usize = 200;
/// The `format` field of an export file.
pub const EXPORT_FORMAT: &str = "page2cam-template/1";

#[derive(Default)]
struct Inner {
    dir: Option<PathBuf>,
    user: BTreeMap<String, Template>,
}

/// Built-in and user templates (cheap to clone; clones share the store).
#[derive(Clone, Default)]
pub struct TemplateLib {
    inner: Arc<RwLock<Inner>>,
}

impl std::fmt::Debug for TemplateLib {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let i = self.inner.read().expect("template lock");
        f.debug_struct("TemplateLib")
            .field("dir", &i.dir)
            .field("user", &i.user.len())
            .finish()
    }
}

impl TemplateLib {
    /// Only the built-ins (user templates kept in memory).
    pub fn builtin_only() -> TemplateLib {
        TemplateLib::default()
    }

    /// Built-ins plus the user templates saved in `dir`; files that no longer
    /// load are skipped and named in the second value.
    pub fn open(dir: PathBuf) -> (TemplateLib, Vec<String>) {
        let mut user = BTreeMap::new();
        let mut bad = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut files: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .collect();
            files.sort();
            for p in files {
                let r = std::fs::read_to_string(&p)
                    .map_err(anyhow::Error::from)
                    .and_then(|t| serde_json::from_str::<Value>(&t).map_err(Into::into))
                    .and_then(Template::from_user_json);
                match r {
                    Ok(t) if builtin(&t.id).is_some() => {
                        bad.push(format!("{}: uses a built-in id", p.display()))
                    }
                    Ok(t) if user.len() < MAX_USER_TEMPLATES => {
                        user.insert(t.id.clone(), t);
                    }
                    Ok(_) => bad.push(format!("{}: more than {MAX_USER_TEMPLATES}", p.display())),
                    Err(e) => bad.push(format!("{}: {e:#}", p.display())),
                }
            }
        }
        let lib = TemplateLib {
            inner: Arc::new(RwLock::new(Inner {
                dir: Some(dir),
                user,
            })),
        };
        (lib, bad)
    }

    /// A template by id (built-ins first).
    pub fn get(&self, id: &str) -> Option<Template> {
        if let Some(t) = builtin(id) {
            return Some(t.clone());
        }
        self.inner.read().ok()?.user.get(id).cloned()
    }

    /// Every template (built-ins, then the user's), optionally of one
    /// category.
    pub fn list(&self, category: Option<&str>) -> Vec<Template> {
        let mut v: Vec<Template> = builtins().to_vec();
        if let Ok(i) = self.inner.read() {
            v.extend(i.user.values().cloned());
        }
        if let Some(c) = category.filter(|c| !c.is_empty()) {
            v.retain(|t| t.category == c);
        }
        v
    }

    /// `{templates: [summary…], categories: [{id, label}…]}` (the API's and
    /// `list_templates`' answer).
    pub fn list_json(&self, category: Option<&str>) -> Value {
        let cats: Vec<Value> = crate::template::CATEGORIES
            .iter()
            .map(|(id, label)| json!({ "id": id, "label": label }))
            .collect();
        let list: Vec<Value> = self
            .list(category)
            .iter()
            .map(Template::summary_json)
            .collect();
        json!({ "templates": list, "categories": cats })
    }

    /// One template in full (params with types, example, HTML).
    pub fn full_json(&self, id: &str) -> Option<Value> {
        self.get(id).map(|t| t.to_json(true))
    }

    /// Whether `id` is a user template.
    pub fn is_user(&self, id: &str) -> bool {
        self.inner
            .read()
            .map(|i| i.user.contains_key(id))
            .unwrap_or(false)
    }

    fn path(dir: &Path, id: &str) -> PathBuf {
        dir.join(format!("{id}.json"))
    }

    /// Create or replace a user template from its definition.
    pub fn save(&self, def: Value) -> Result<Template> {
        let t = Template::from_user_json(def)?;
        ensure!(
            builtin(&t.id).is_none(),
            "{:?} is a built-in template: choose another id (or copy it with copy_of)",
            t.id
        );
        let mut i = self
            .inner
            .write()
            .map_err(|_| anyhow!("template store poisoned"))?;
        ensure!(
            i.user.contains_key(&t.id) || i.user.len() < MAX_USER_TEMPLATES,
            "at most {MAX_USER_TEMPLATES} templates of your own (delete some first)"
        );
        if let Some(dir) = &i.dir {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            let p = Self::path(dir, &t.id);
            let tmp = p.with_extension("json.part");
            let body = serde_json::to_vec_pretty(&t)?;
            std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
            std::fs::rename(&tmp, &p).with_context(|| format!("writing {}", p.display()))?;
        }
        i.user.insert(t.id.clone(), t.clone());
        Ok(t)
    }

    /// Delete a user template (built-ins cannot be deleted).
    pub fn delete(&self, id: &str) -> Result<()> {
        ensure!(
            builtin(id).is_none(),
            "{id:?} is a built-in template and cannot be deleted"
        );
        let mut i = self
            .inner
            .write()
            .map_err(|_| anyhow!("template store poisoned"))?;
        if i.user.remove(id).is_none() {
            bail!("no template {id:?} (list_templates shows the ids)");
        }
        if let Some(dir) = &i.dir {
            let _ = std::fs::remove_file(Self::path(dir, id));
        }
        Ok(())
    }

    /// A template as an export file (`.p2c-template.json`).
    pub fn export(&self, id: &str) -> Result<Value> {
        let t = self
            .get(id)
            .ok_or_else(|| anyhow!("no template {id:?} (list_templates shows the ids)"))?;
        let mut v = serde_json::to_value(&t)?;
        if let Some(m) = v.as_object_mut() {
            m.remove("builtin");
            m.insert("format".into(), json!(EXPORT_FORMAT));
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(id: &str) -> Value {
        json!({"id": id, "name": "Mine", "html": "<b id=b></b><script>P2C.onParams(function(p){P2C.text(P2C.$('#b'), p.t)})</script>",
            "params": [{"name": "t", "type": "text", "default": "hi"}]})
    }

    #[test]
    fn user_templates_crud_limits_and_builtin_collision() {
        let d = tempfile::tempdir().unwrap();
        let (lib, bad) = TemplateLib::open(d.path().join("templates"));
        assert!(bad.is_empty());
        let t = lib.save(def("my-card")).unwrap();
        assert!(!t.builtin);
        assert!(lib.is_user("my-card"));
        assert!(d.path().join("templates/my-card.json").exists());
        assert!(lib.list(Some("mine")).iter().any(|t| t.id == "my-card"));
        assert!(lib.list(None).len() > builtins().len());
        let e = lib.save(def("lower-third")).unwrap_err().to_string();
        assert!(e.contains("built-in"), "{e}");
        assert!(lib
            .delete("lower-third")
            .unwrap_err()
            .to_string()
            .contains("built-in"));
        // Reopened from disk.
        let (again, bad) = TemplateLib::open(d.path().join("templates"));
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(again.get("my-card").unwrap().params[0].name, "t");
        again.delete("my-card").unwrap();
        assert!(again.get("my-card").is_none());
        assert!(!d.path().join("templates/my-card.json").exists());
        assert!(again.delete("my-card").is_err());
        // The limit.
        let mem = TemplateLib::builtin_only();
        for n in 0..MAX_USER_TEMPLATES {
            mem.save(def(&format!("t{n}"))).unwrap();
        }
        assert!(mem
            .save(def("one-more"))
            .unwrap_err()
            .to_string()
            .contains("at most"));
        mem.save(def("t0")).unwrap();
        // A broken file is skipped and named.
        std::fs::write(d.path().join("templates/bad.json"), "{").unwrap();
        let (_, bad) = TemplateLib::open(d.path().join("templates"));
        assert_eq!(bad.len(), 1);
    }

    #[test]
    fn import_export_round_trip() {
        let lib = TemplateLib::builtin_only();
        lib.save(def("card")).unwrap();
        let file = lib.export("card").unwrap();
        assert_eq!(file["format"], EXPORT_FORMAT);
        let other = TemplateLib::builtin_only();
        let mut f = file.clone();
        f["id"] = json!("card-2");
        let t = other.save(f).unwrap();
        assert_eq!(t.html, lib.get("card").unwrap().html);
        // A built-in exports too (and imports under a new id).
        let mut b = lib.export("lower-third").unwrap();
        b["id"] = json!("my-lower-third");
        assert!(other.save(b).is_ok());
    }
}
