use super::*;

/// The `add_layer {…}` example calls in a text.
fn example_calls(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("add_layer "))
        .map(|j| serde_json::from_str(j).unwrap_or_else(|e| panic!("bad example call {j}: {e}")))
        .collect()
}

#[test]
fn every_playbook_and_howto_exists() {
    let ids: Vec<&str> = playbooks().iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, PLAYBOOK_IDS, "playbook files");
    let mut h: Vec<&str> = howtos().iter().map(|d| d.id.as_str()).collect();
    h.sort();
    let mut want = HOWTO_IDS.to_vec();
    want.sort();
    assert_eq!(h, want);
    for d in playbooks() {
        assert!(
            GROUPS.iter().any(|(g, _)| *g == d.group),
            "{}: group {:?}",
            d.id,
            d.group
        );
        assert!(!d.title.is_empty() && d.title != d.id, "{}: title", d.id);
        assert!(!d.prompt.is_empty(), "{}: prompt", d.id);
        assert!(d.aliases.len() >= 3, "{}: aliases", d.id);
        assert!(
            d.body.starts_with("# "),
            "{}: body starts with a heading",
            d.id
        );
    }
    // Every group has playbooks; software companies have four.
    for (g, _) in GROUPS {
        assert!(playbooks().iter().any(|d| d.group == *g), "{g}");
    }
    assert_eq!(
        playbooks().iter().filter(|d| d.group == "software").count(),
        4
    );
}

#[test]
fn playbooks_reference_real_templates() {
    for d in playbooks() {
        let calls = example_calls(&d.body);
        assert!(
            calls.len() >= 3,
            "{}: {} example add_layer calls",
            d.id,
            calls.len()
        );
        // Every template call goes through a real layer list (field names,
        // geometry, ids, params).
        let mut set = p2c_overlay::LayerSet::new();
        let assets = p2c_overlay::Assets::new(std::env::temp_dir().join("p2c-guide-test-assets"));
        for c in calls.iter().filter(|c| c["kind"] == "template") {
            set.add(c.clone(), &assets, std::time::Instant::now())
                .unwrap_or_else(|e| panic!("{}: {c}: {e:#}", d.id));
        }
        for c in calls {
            if c["kind"] != "template" {
                continue;
            }
            let id = c["template"].as_str().unwrap();
            let t = p2c_overlay::builtin(id)
                .unwrap_or_else(|| panic!("{}: unknown template {id}", d.id));
            let params = c["params"].as_object().cloned().unwrap_or_default();
            t.check_params(&params)
                .unwrap_or_else(|e| panic!("{}: {id}: {e:#}", d.id));
        }
        for sec in ["## Background", "## Steps", "## Rules"] {
            assert!(d.body.contains(sec), "{}: no {sec:?} section", d.id);
        }
    }
}

#[test]
fn regulated_rules_present() {
    let body = |id: &str| playbook(id).unwrap().body.to_lowercase();
    for id in ["telehealth", "therapy", "legal-consulting", "finance"] {
        let b = body(id);
        assert!(
            b.contains("blurred-room") || b.contains("blurred room"),
            "{id}: blurred room"
        );
        assert!(b.contains("never"), "{id}: privacy rule");
    }
    assert!(body("finance").contains("disclaimer-bar"));
    assert!(body("legal-consulting").contains("confidential-badge"));
    assert!(body("therapy").contains("flash"));
    for d in playbooks() {
        assert!(d.body.contains("snapshot"), "{}: snapshot", d.id);
    }
}

#[test]
fn every_topic_resolves() {
    for id in PLAYBOOK_IDS.iter().chain(HOWTO_IDS) {
        assert_eq!(topic(id).unwrap().0, *id);
    }
    for t in builtins() {
        let (id, text) = topic(&t.id).unwrap();
        assert_eq!(id, t.id);
        assert!(text.contains("add_layer"), "{id}");
    }
    assert_eq!(topic("").unwrap().0, "index");
    assert_eq!(topic("catalogue").unwrap().0, "catalogue");
    for (q, want) in [
        ("doctor", "telehealth"),
        ("Doctor appointment", "telehealth"),
        ("youtuber", "streaming"),
        ("I'm a physiotherapist", "telehealth"),
        ("webinar", "webinar"),
        ("Webinars", "webinar"),
        ("release demo", "software-release"),
        ("sprint review", "engineering-rituals"),
        ("live coding", "dev-stream"),
        ("teacher", "teaching"),
        ("church", "faith"),
        ("Lower Third", "lower-third"),
    ] {
        assert_eq!(topic(q).map(|t| t.0), Ok(want.to_string()), "{q}");
    }
    let e = topic("zzqx").unwrap_err();
    assert!(e.contains("telehealth"), "{e}");
}

#[test]
fn topics_are_small() {
    for (id, text) in all_topics() {
        assert!(text.len() <= 8 * 1024, "{id}: {} bytes", text.len());
    }
}

#[test]
fn instructions_short() {
    let n = instructions().lines().count();
    assert!(n <= 60, "{n} lines");
    for w in [
        "status",
        "snapshot",
        "guide",
        "template",
        "safe area",
        "save_scene",
    ] {
        assert!(instructions().contains(w), "{w}");
    }
}

#[test]
fn resources_and_prompts() {
    let rs = resources();
    assert_eq!(
        rs.len(),
        2 + HOWTO_IDS.len() + PLAYBOOK_IDS.len() + builtins().len()
    );
    for r in &rs {
        assert!(read_resource(&r.uri).is_some(), "{}", r.uri);
    }
    assert!(read_resource("page2cam://nope").is_none());
    assert!(read_resource("page2cam://templates/../x").is_none());
    let ps = prompts();
    assert_eq!(ps.len(), PLAYBOOK_IDS.len() + 2);
    let mut args = Map::new();
    args.insert("name".into(), json!("Dr. Lee"));
    let (_, text) = prompt("telehealth", &args).unwrap();
    assert!(text.contains("guide with topic \"telehealth\"") && text.contains("Dr. Lee"));
    assert!(prompt("clear-graphics", &Map::new())
        .unwrap()
        .1
        .contains("clear_layers"));
    assert!(prompt("save-look", &Map::new())
        .unwrap()
        .1
        .contains("save_scene"));
    assert!(prompt("nope", &Map::new()).is_none());
}

#[test]
fn skill_files_are_well_formed() {
    let f = skill_files();
    let skill = &f.iter().find(|(p, _)| p == "SKILL.md").unwrap().1;
    assert!(skill.starts_with("---\nname: page2cam\ndescription: "));
    let desc = skill.lines().nth(2).unwrap();
    assert!(desc.len() <= 1024 + 13, "description too long");
    for (p, _) in &f {
        assert!(
            !p.contains("..") && !p.starts_with('/') && !p.contains('\\'),
            "{p}"
        );
    }
    assert!(f
        .iter()
        .any(|(p, _)| p == "reference/playbooks/telehealth.md"));
}
