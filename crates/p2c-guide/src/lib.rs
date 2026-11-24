//! What agents read about Page2Cam, from one source: the MCP
//! `instructions`, the `guide` tool's topics (playbooks per user group,
//! how-tos, the template catalogue and each template's reference), MCP
//! resources and prompts, the Claude Code skill files and
//! `docs/agent-guide/`.
//!
//! Playbooks and how-tos are Markdown files under `content/` with a short
//! header (`key: value` lines between `---` lines); template references
//! are generated from the built-in templates of `p2c-overlay`.
use std::sync::OnceLock;

use p2c_overlay::template::{builtins, Template, CATEGORIES};
use serde_json::{json, Map, Value};

mod content {
    include!(concat!(env!("OUT_DIR"), "/content.rs"));
}

/// The MCP `instructions` text.
pub fn instructions() -> &'static str {
    include_str!("../content/instructions.md").trim_end()
}

/// User groups, in the order playbooks are listed: `(id, label)`.
pub const GROUPS: &[(&str, &str)] = &[
    ("creators", "Creators"),
    ("business", "Business"),
    ("care", "Care and education"),
    ("community", "Community"),
    ("software", "Software companies"),
];

/// Every playbook id, by group.
pub const PLAYBOOK_IDS: &[&str] = &[
    "streaming",
    "podcast",
    "music",
    "cooking-diy",
    "fitness",
    "sales-demo",
    "webinar",
    "live-shopping",
    "team-meeting",
    "interview",
    "real-estate",
    "finance",
    "legal-consulting",
    "it-support",
    "telehealth",
    "therapy",
    "teaching",
    "faith",
    "virtual-event",
    "fundraiser",
    "software-release",
    "engineering-rituals",
    "dev-stream",
    "customer-success",
];

/// Every how-to id.
pub const HOWTO_IDS: &[&str] = &[
    "templates",
    "scenes",
    "layout",
    "privacy",
    "writing-templates",
    "performance",
];

/// A playbook or how-to: its header and Markdown body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    pub id: String,
    pub title: String,
    /// Playbooks: a [`GROUPS`] id.
    pub group: String,
    /// Words that lead `guide` here ("doctor" → telehealth).
    pub aliases: Vec<String>,
    /// Playbooks: the MCP prompt's title ("Set up for a webinar").
    pub prompt: String,
    /// Playbooks: the prompt's optional arguments.
    pub args: Vec<String>,
    pub body: String,
}

fn parse(id: &str, text: &str) -> Doc {
    let text = text.replace("\r\n", "\n");
    let mut doc = Doc {
        id: id.to_string(),
        title: id.to_string(),
        group: String::new(),
        aliases: Vec::new(),
        prompt: String::new(),
        args: Vec::new(),
        body: text.trim().to_string(),
    };
    let Some(rest) = text.strip_prefix("---\n") else {
        return doc;
    };
    let Some((head, body)) = rest.split_once("\n---\n") else {
        return doc;
    };
    let list = |v: &str| -> Vec<String> {
        v.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };
    for line in head.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "title" => doc.title = v.to_string(),
            "group" => doc.group = v.to_string(),
            "aliases" => doc.aliases = list(v),
            "prompt" => doc.prompt = v.to_string(),
            "args" => doc.args = list(v),
            _ => {}
        }
    }
    doc.body = body.trim().to_string();
    doc
}

/// The playbooks (by [`PLAYBOOK_IDS`] order).
pub fn playbooks() -> &'static [Doc] {
    static ALL: OnceLock<Vec<Doc>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut v: Vec<Doc> = content::PLAYBOOKS
            .iter()
            .map(|(id, t)| parse(id, t))
            .collect();
        v.sort_by_key(|d| {
            PLAYBOOK_IDS
                .iter()
                .position(|p| *p == d.id)
                .unwrap_or(usize::MAX)
        });
        v
    })
}

/// The how-tos.
pub fn howtos() -> &'static [Doc] {
    static ALL: OnceLock<Vec<Doc>> = OnceLock::new();
    ALL.get_or_init(|| content::HOWTOS.iter().map(|(id, t)| parse(id, t)).collect())
}

pub fn playbook(id: &str) -> Option<&'static Doc> {
    playbooks().iter().find(|d| d.id == id)
}

pub fn howto(id: &str) -> Option<&'static Doc> {
    howtos().iter().find(|d| d.id == id)
}

fn group_label(id: &str) -> &str {
    GROUPS
        .iter()
        .find(|(g, _)| *g == id)
        .map_or(id, |(_, l)| *l)
}

fn category_label(id: &str) -> &str {
    CATEGORIES
        .iter()
        .find(|(c, _)| *c == id)
        .map_or(id, |(_, l)| *l)
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// The `guide` index: playbooks by group, the template catalogue in
/// short, the how-tos.
pub fn index() -> String {
    let mut s = String::from(
        "# Page2Cam guide\n\nCall `guide` with a topic: a playbook id (or just the user's role or goal, e.g. \"doctor\", \"webinar\", \"release demo\"), a template id, `catalogue` (every template with a line each) or a how-to.\n\n## Playbooks (what to set up for whom)\n\n",
    );
    for (g, label) in GROUPS {
        let items: Vec<String> = playbooks()
            .iter()
            .filter(|d| d.group == *g)
            .map(|d| format!("`{}` ({})", d.id, d.title))
            .collect();
        if !items.is_empty() {
            s.push_str(&format!("- **{label}:** {}\n", items.join(", ")));
        }
    }
    s.push_str("\n## Templates (add_layer kind \"template\")\n\n");
    for (c, label) in CATEGORIES {
        let ids: Vec<String> = builtins()
            .iter()
            .filter(|t| t.category == *c)
            .map(|t| format!("`{}`", t.id))
            .collect();
        if !ids.is_empty() {
            s.push_str(&format!("- **{label}:** {}\n", ids.join(", ")));
        }
    }
    s.push_str("\nThe user's own templates: `list_templates`. One template in full: `guide` with its id, or `get_template`.\n\n## How-tos\n\n");
    for d in howtos() {
        s.push_str(&format!("- `{}`: {}\n", d.id, d.title));
    }
    s
}

/// Every built-in template with a line each, by category.
pub fn catalogue() -> String {
    let mut s = String::from("# Template catalogue\n\nAdd one with `add_layer {\"kind\": \"template\", \"template\": \"<id>\", \"params\": {…}}`; change values live with `update_layer {\"id\": \"<layer id>\", \"params\": {…}}`. Every template also takes `theme` (brand, news, minimal) and `accent` (#rrggbb). Full parameters: `guide` with the template id.\n");
    for (c, label) in CATEGORIES {
        let ts: Vec<&Template> = builtins().iter().filter(|t| t.category == *c).collect();
        if ts.is_empty() {
            continue;
        }
        s.push_str(&format!("\n## {label}\n\n"));
        for t in ts {
            let ff = if t.full_frame { " (full frame)" } else { "" };
            s.push_str(&format!("- `{}` {}{ff}: {}\n", t.id, t.name, t.description));
        }
    }
    s
}

/// `v` with object keys in sorted order, whatever serde_json's
/// `preserve_order` feature (another crate in the build may turn it on), so
/// generated texts are the same in every build.
fn sorted(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), sorted(&m[k])))
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
        x => x.clone(),
    }
}

fn compact(v: &Value) -> String {
    serde_json::to_string(&sorted(v)).unwrap_or_default()
}

/// One template's reference: parameters, an example call, a live change.
pub fn template_topic(t: &Template) -> String {
    let (x, y, w, h) = t.geometry();
    let place = if t.full_frame {
        "covers the whole output (a full-frame card)".to_string()
    } else {
        format!("x {x}, y {y}, w {w}, h {h} (0..1 of the 1920x1080 output; give x/y/w/h to move or resize)")
    };
    let mut s = format!(
        "# {} (`{}`)\n\n{}\n\nCategory: {}. Default place: {place}.\n\n## Parameters\n\n| name | type | default | notes |\n|---|---|---|---|\n",
        t.name,
        t.id,
        t.description,
        category_label(&t.category)
    );
    for p in t.all_params() {
        let mut notes = p.help.clone();
        if p.required {
            notes = format!("**required.** {notes}");
        }
        if !p.options.is_empty() {
            notes.push_str(&format!(" Options: {}.", p.options.join(", ")));
        }
        if !p.columns.is_empty() {
            notes.push_str(&format!(" Rows of [{}].", p.columns.join(", ")));
        }
        if let (Some(lo), Some(hi)) = (p.min, p.max) {
            notes.push_str(&format!(" {lo} to {hi}."));
        }
        s.push_str(&format!(
            "| `{}` | {} | `{}` | {} |\n",
            p.name,
            p.kind.as_str(),
            cell(&compact(&p.default_value())),
            cell(&notes)
        ));
    }
    let add = json!({ "kind": "template", "template": t.id, "params": t.example });
    s.push_str(&format!(
        "\n## Add it\n\n```\nadd_layer {}\n```\n",
        compact(&add)
    ));
    if !t.update.is_empty() {
        let upd = json!({ "id": t.id, "params": t.update });
        s.push_str(&format!(
            "\n## Change it live (no reload: timers and animations keep running)\n\n```\nupdate_layer {}\n```\n\nOnly the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).\n",
            compact(&upd)
        ));
    }
    s
}

/// The ids `guide` answers for, with their text.
fn all_topics() -> Vec<(String, String)> {
    let mut v = vec![
        ("index".to_string(), index()),
        ("catalogue".to_string(), catalogue()),
    ];
    for d in playbooks().iter().chain(howtos()) {
        v.push((d.id.clone(), d.body.clone()));
    }
    for t in builtins() {
        v.push((t.id.clone(), template_topic(t)));
    }
    v
}

fn norm(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn stem(w: &str) -> &str {
    let w = w.strip_suffix("ing").filter(|s| s.len() >= 4).unwrap_or(w);
    w.strip_suffix('s').filter(|s| s.len() >= 3).unwrap_or(w)
}

/// Words of a candidate topic for fuzzy matching (id, title, aliases).
fn words_of(id: &str, title: &str, aliases: &[String]) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        for w in norm(s).split('-') {
            if w.len() >= 2 {
                v.push(stem(w).to_string());
            }
        }
    };
    add(id);
    add(title);
    for a in aliases {
        add(a);
    }
    v
}

/// The text for `q`: an id (playbook, how-to, template, `index`,
/// `catalogue`), an alias, or the best word match ("doctor appointment" →
/// telehealth). `Err` lists what exists.
pub fn topic(q: &str) -> Result<(String, String), String> {
    let n = norm(q);
    if n.is_empty() || n == "guide" || n == "help" {
        return Ok(("index".into(), index()));
    }
    let topics = all_topics();
    if let Some(t) = topics.iter().find(|(id, _)| *id == n) {
        return Ok(t.clone());
    }
    if matches!(
        n.as_str(),
        "templates-list" | "catalog" | "template-catalogue" | "list"
    ) {
        return Ok(("catalogue".into(), catalogue()));
    }
    // Aliases (whole phrases).
    for d in playbooks().iter().chain(howtos()) {
        if d.aliases.iter().any(|a| norm(a) == n) {
            return Ok((d.id.clone(), d.body.clone()));
        }
    }
    // Word overlap: aliases and ids weigh more than titles.
    let qw: Vec<String> = n
        .split('-')
        .filter(|w| w.len() >= 2)
        .map(|w| stem(w).to_string())
        .collect();
    let mut best: Option<(usize, String, String)> = None;
    let mut consider = |id: &str, title: &str, aliases: &[String], text: &dyn Fn() -> String| {
        let ws = words_of(id, title, aliases);
        let score: usize = qw
            .iter()
            .map(|w| {
                ws.iter()
                    .filter(|x| *x == w || (w.len() >= 4 && x.starts_with(w.as_str())))
                    .count()
                    .min(1)
            })
            .sum();
        if score > 0 && best.as_ref().is_none_or(|(b, _, _)| score > *b) {
            best = Some((score, id.to_string(), text()));
        }
    };
    for d in playbooks().iter().chain(howtos()) {
        consider(&d.id, &d.title, &d.aliases, &|| d.body.clone());
    }
    for t in builtins() {
        consider(&t.id, &t.name, &[], &|| template_topic(t));
    }
    if let Some((_, id, text)) = best {
        return Ok((id, text));
    }
    Err(format!(
        "no guide topic {q:?}. Playbooks: {}. How-tos: {}. Or a template id (`catalogue` lists them), or no topic for the index.",
        PLAYBOOK_IDS.join(", "),
        HOWTO_IDS.join(", ")
    ))
}

// ------------------------------------------------------------ MCP resources

/// One MCP resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    pub uri: String,
    pub name: String,
    pub title: String,
    pub description: String,
}

/// Every resource: the guide, the catalogue, how-tos, playbooks, templates.
pub fn resources() -> Vec<Resource> {
    let r = |uri: String, name: &str, title: &str, description: &str| Resource {
        uri,
        name: name.to_string(),
        title: title.to_string(),
        description: description.to_string(),
    };
    let mut v = vec![
        r(
            "page2cam://guide".into(),
            "guide",
            "Page2Cam guide",
            "Index: playbooks per user group, templates, how-tos",
        ),
        r(
            "page2cam://catalogue".into(),
            "catalogue",
            "Template catalogue",
            "Every built-in overlay template with a line each",
        ),
    ];
    for d in howtos() {
        v.push(r(
            format!("page2cam://howto/{}", d.id),
            &d.id,
            &d.title,
            "How-to",
        ));
    }
    for d in playbooks() {
        v.push(r(
            format!("page2cam://playbooks/{}", d.id),
            &d.id,
            &d.title,
            &format!("Playbook ({})", group_label(&d.group)),
        ));
    }
    for t in builtins() {
        v.push(r(
            format!("page2cam://templates/{}", t.id),
            &t.id,
            &t.name,
            &t.description,
        ));
    }
    v
}

/// A resource's Markdown text.
pub fn read_resource(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("page2cam://")?;
    match rest.split_once('/') {
        None if rest == "guide" => Some(index()),
        None if rest == "catalogue" => Some(catalogue()),
        Some(("howto", id)) => howto(id).map(|d| d.body.clone()),
        Some(("playbooks", id)) => playbook(id).map(|d| d.body.clone()),
        Some(("templates", id)) => p2c_overlay::builtin(id).map(template_topic),
        _ => None,
    }
}

// ------------------------------------------------------------ MCP prompts

fn arg_help(a: &str) -> &'static str {
    match a {
        "name" => "Your name as it should appear on screen",
        "title" => "Your role or title",
        "company" => "Company or organisation",
        "topic" => "What the session is about",
        "product" => "The product or release",
        "version" => "The version you are showing",
        "event" => "The event's name",
        "cause" => "What you raise money for",
        "subject" => "Subject or class",
        _ => "Optional detail",
    }
}

/// `prompts/list` entries: one per playbook plus "clear-graphics" and
/// "save-look".
pub fn prompts() -> Vec<Value> {
    let mut v: Vec<Value> = playbooks()
        .iter()
        .map(|d| {
            let args: Vec<Value> = d
                .args
                .iter()
                .map(|a| json!({ "name": a, "description": arg_help(a), "required": false }))
                .collect();
            json!({
                "name": d.id,
                "title": d.prompt,
                "description": format!("{} ({})", d.prompt, d.title),
                "arguments": args,
            })
        })
        .collect();
    v.push(
        json!({ "name": "clear-graphics", "title": "Clear all graphics",
        "description": "Hide every overlay graphic", "arguments": [] }),
    );
    v.push(json!({ "name": "save-look", "title": "Save this look",
        "description": "Save the current graphics (and background and webcam) as a scene",
        "arguments": [{ "name": "name", "description": "Name of the look, e.g. Intro", "required": false }] }));
    v
}

/// A prompt's description and text, with the user's arguments filled in.
pub fn prompt(name: &str, args: &Map<String, Value>) -> Option<(String, String)> {
    let given: Vec<String> = args
        .iter()
        .filter_map(|(k, v)| {
            let s = v.as_str()?.trim();
            (!s.is_empty()).then(|| format!("- {k}: {}", s.chars().take(300).collect::<String>()))
        })
        .collect();
    let details = if given.is_empty() {
        "The user gave no details: ask for anything you need (names, titles, links) before showing it in a call, or use neutral placeholders and say so.".to_string()
    } else {
        format!("Details from the user:\n{}", given.join("\n"))
    };
    match name {
        "clear-graphics" => Some((
            "Clear all graphics".into(),
            "Use the page2cam tools: call clear_layers to hide every overlay graphic (they stay in the list), then snapshot to check that nothing covers the picture. Tell the user what was hidden.".into(),
        )),
        "save-look" => {
            let n = args.get("name").and_then(Value::as_str).unwrap_or("").trim();
            let n = if n.is_empty() { "a short name the user chooses (ask)".to_string() } else { format!("{n:?}") };
            Some((
                "Save this look".into(),
                format!("Use the page2cam tools: call list_layers to see the current graphics, then save_scene with name {n}, include_background true and include_webcam true. Then list_scenes and tell the user how to bring it back (load_scene)."),
            ))
        }
        _ => {
            let d = playbook(name)?;
            Some((
                d.prompt.clone(),
                format!(
                    "{prompt} with Page2Cam ({title}).\n\nFirst call the page2cam tool guide with topic \"{id}\" and follow that playbook. Work step by step: status (start Page2Cam if it is stopped), then background and webcam, then the graphics in the playbook's order. After each visible change call snapshot and check it: nothing covers the face, text sits inside the safe area, at most one thing moves. Keep the privacy rules of the playbook and the guide topic \"privacy\". Everything you change is seen live by the people in the call or stream.\n\n{details}\n\nWhen it looks right, offer to save it as a scene (save_scene) so it can be brought back in one call.",
                    prompt = d.prompt,
                    title = d.title,
                    id = d.id,
                ),
            ))
        }
    }
}

// ------------------------------------------------------------ files

const SKILL_DESCRIPTION: &str = "Run the user's camera show with Page2Cam (virtual camera for Teams/Zoom/Meet and live streams): web pages and screens behind the person, webcam cut-out and backgrounds, and overlay graphics such as lower thirds, name badges, countdowns, agendas, charts, polls, QR codes, product cards, code and release-notes cards. Use when the user asks to set up their webcam look, a meeting, call, webinar, class, telehealth visit, live stream or product demo, to add or change on-screen titles, timers or graphics, or mentions Page2Cam.";

fn skill_md() -> String {
    let mut s = format!(
        "---\nname: page2cam\ndescription: {SKILL_DESCRIPTION}\n---\n\n# Page2Cam\n\nControl Page2Cam through the `page2cam` MCP server's tools (status, start, set_background, set_overlay, add_layer, update_layer, snapshot, save_scene, guide, …). If those tools are missing, ask the user to turn on \"Allow agents\" in the Page2Cam tray menu and restart this app.\n\n{}\n\n## Reference files\n\n- `reference/guide.md`: the index (playbooks, templates, how-tos)\n- `reference/catalogue.md`: every built-in template with a line each\n- `reference/playbooks/<id>.md`: one per user group\n- `reference/templates/<id>.md`: one template's parameters and example calls\n- `reference/howto/<id>.md`: templates, scenes, layout, privacy, writing-templates, performance\n\nThe same texts come from the `guide` tool; prefer it when the server is newer than these files.\n",
        instructions()
    );
    s.push('\n');
    s
}

/// The Claude Code skill's files: `(path relative to the skill folder,
/// text)`.
pub fn skill_files() -> Vec<(String, String)> {
    let mut v = vec![
        ("SKILL.md".to_string(), skill_md()),
        ("reference/guide.md".to_string(), index()),
        ("reference/catalogue.md".to_string(), catalogue()),
    ];
    for d in playbooks() {
        v.push((
            format!("reference/playbooks/{}.md", d.id),
            format!("{}\n", d.body),
        ));
    }
    for d in howtos() {
        v.push((
            format!("reference/howto/{}.md", d.id),
            format!("{}\n", d.body),
        ));
    }
    for t in builtins() {
        v.push((
            format!("reference/templates/{}.md", t.id),
            template_topic(t),
        ));
    }
    v
}

/// `docs/agent-guide/`: `(path relative to it, text)`.
pub fn docs_files() -> Vec<(String, String)> {
    let mut readme = String::from(
        "# Agent guide\n\n<!-- Generated from crates/p2c-guide/content and the built-in templates (crates/p2c-overlay/templates). Do not edit: change the sources and run `P2C_WRITE_DOCS=1 cargo test -p p2c-guide --test docs_sync`. -->\n\nWhat AI agents read about Page2Cam: the same texts reach them through the MCP `guide` tool, MCP resources and prompts, and the Claude Code skill. See also [agent-api.md](../agent-api.md).\n\n- [MCP instructions](instructions.md) (sent to every agent when it connects)\n- [Template catalogue](catalogue.md)\n\n## Playbooks\n\n",
    );
    for (g, label) in GROUPS {
        let items: Vec<String> = playbooks()
            .iter()
            .filter(|d| d.group == *g)
            .map(|d| format!("[{}](playbooks/{}.md)", d.title, d.id))
            .collect();
        if !items.is_empty() {
            readme.push_str(&format!("- **{label}:** {}\n", items.join(", ")));
        }
    }
    readme.push_str("\n## How-tos\n\n");
    for d in howtos() {
        readme.push_str(&format!("- [{}](howto/{}.md)\n", d.title, d.id));
    }
    readme.push_str("\n## Templates\n\n");
    for (c, label) in CATEGORIES {
        let items: Vec<String> = builtins()
            .iter()
            .filter(|t| t.category == *c)
            .map(|t| format!("[{}](templates/{}.md)", t.id, t.id))
            .collect();
        if !items.is_empty() {
            readme.push_str(&format!("- **{label}:** {}\n", items.join(", ")));
        }
    }
    let mut v = vec![
        ("README.md".to_string(), readme),
        (
            "instructions.md".to_string(),
            format!("{}\n", instructions()),
        ),
        ("catalogue.md".to_string(), catalogue()),
    ];
    for d in playbooks() {
        v.push((format!("playbooks/{}.md", d.id), format!("{}\n", d.body)));
    }
    for d in howtos() {
        v.push((format!("howto/{}.md", d.id), format!("{}\n", d.body)));
    }
    for t in builtins() {
        v.push((format!("templates/{}.md", t.id), template_topic(t)));
    }
    v
}

#[cfg(test)]
mod tests;
