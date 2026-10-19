//! Laying a freshly serialized config onto the file already on disk, so a
//! save changes values and keeps the comments a person wrote around them.

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

/// `new_text`'s values laid onto `old_text`: every value comes from
/// `new_text`; comments in `old_text` stay with the key, table, or array
/// element they sit on. An `old_text` that does not parse yields `new_text`.
pub fn merge(old_text: &str, new_text: &str) -> String {
    let (Ok(mut old), Ok(new)) = (
        old_text.parse::<DocumentMut>(),
        new_text.parse::<DocumentMut>(),
    ) else {
        return new_text.to_string();
    };
    merge_table(old.as_table_mut(), new.as_table());
    old.to_string()
}

fn merge_table(old: &mut Table, new: &Table) {
    let gone: Vec<String> = old
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| !new.contains_key(key))
        .collect();
    for key in gone {
        old.remove(&key);
    }
    for (key, new_item) in new.iter() {
        match (old.get_mut(key), new_item) {
            (Some(Item::Table(o)), Item::Table(n)) => {
                merge_table(o, n);
                // The new document's order, so a reorder is saved as one.
                o.set_position(n.position());
            }
            (Some(Item::ArrayOfTables(o)), Item::ArrayOfTables(n)) => *o = merge_array(o, n),
            (Some(Item::Value(o)), Item::Value(n)) => {
                let decor = o.decor().clone();
                *o = n.clone();
                *o.decor_mut() = decor;
            }
            _ => {
                old.insert(key, new_item.clone());
            }
        }
    }
}

/// The field an array element is known by, so its comments follow it.
fn identity(table: &Table) -> Option<(&'static str, String)> {
    ["id", "name", "key", "path"].into_iter().find_map(|field| {
        table
            .get(field)
            .and_then(|v| v.as_str())
            .map(|s| (field, s.to_string()))
    })
}

fn merge_array(old: &ArrayOfTables, new: &ArrayOfTables) -> ArrayOfTables {
    let mut out = ArrayOfTables::new();
    for n in new.iter() {
        let matched =
            identity(n).and_then(|id| old.iter().find(|o| identity(o).as_ref() == Some(&id)));
        match matched {
            Some(o) => {
                let mut o = o.clone();
                merge_table(&mut o, n);
                o.set_position(n.position());
                out.push(o);
            }
            None => out.push(n.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Config;

    const OLD: &str = "# mine\nschema_version = 2 # v\nindexes = []\n\n[settings]\n# pick dark\ntheme = \"dark\"\n\n# the api one\n[[projects]]\nid = \"a\"\nlabel = \"api\"\n\n# the web one\n[[projects]]\nid = \"b\"\nlabel = \"web\"\n";

    #[test]
    fn a_comment_survives_a_value_change() {
        let new = OLD
            .replace("theme = \"dark\"", "theme = \"light\"")
            .replace("# pick dark\n", "");
        let merged = merge(OLD, &new);
        assert!(
            merged.contains("# pick dark\ntheme = \"light\""),
            "{merged}"
        );
        assert!(merged.contains("schema_version = 2 # v"), "{merged}");
    }

    #[test]
    fn a_projects_comment_follows_it_when_reordered() {
        let new = "schema_version = 2\nindexes = []\n\n[settings]\ntheme = \"dark\"\n\n[[projects]]\nid = \"b\"\nlabel = \"web\"\n\n[[projects]]\nid = \"a\"\nlabel = \"api\"\n";
        let merged = merge(OLD, new);
        let web = merged.find("# the web one").unwrap();
        let api = merged.find("# the api one").unwrap();
        assert!(web < api, "{merged}");
        assert!(
            merged[web..].starts_with("# the web one\n[[projects]]\nid = \"b\""),
            "{merged}"
        );
    }

    #[test]
    fn removing_a_project_removes_its_comment() {
        let new = "schema_version = 2\nindexes = []\n\n[settings]\ntheme = \"dark\"\n\n[[projects]]\nid = \"a\"\nlabel = \"api\"\n";
        let merged = merge(OLD, new);
        assert!(!merged.contains("the web one"), "{merged}");
        assert!(merged.contains("# the api one"), "{merged}");
    }

    #[test]
    fn an_unparseable_old_file_gives_the_new_text() {
        assert_eq!(merge("this = = broken", "a = 1\n"), "a = 1\n");
    }

    fn sample() -> Config {
        let mut c = Config::default();
        c.projects.push(crate::model::Project {
            label: "api".into(),
            pyproject_path: "/p/api/pyproject.toml".into(),
            ..Default::default()
        });
        c
    }

    #[test]
    fn the_merged_text_parses_to_exactly_the_new_config() {
        let old = format!(
            "# hand note\n{}",
            toml::to_string_pretty(&sample()).unwrap()
        );
        let mut new = sample();
        new.projects[0].label = "renamed".into();
        new.settings.theme = crate::model::ThemeChoice::Dark;
        let merged = merge(&old, &toml::to_string_pretty(&new).unwrap());
        assert_eq!(toml::from_str::<Config>(&merged).unwrap(), new);
        assert!(merged.starts_with("# hand note"), "{merged}");
    }

    #[test]
    fn an_empty_list_becoming_tables_round_trips() {
        let empty = Config::default();
        let mut full = Config::default();
        full.indexes.push(crate::model::Index {
            name: "corp".into(),
            url: "https://corp/simple".into(),
            kind: crate::model::IndexKind::Extra,
            default_for_new_projects: false,
            auth: crate::model::IndexAuth::Token,
        });
        let e = toml::to_string_pretty(&empty).unwrap();
        let f = toml::to_string_pretty(&full).unwrap();
        assert_eq!(toml::from_str::<Config>(&merge(&e, &f)).unwrap(), full);
        assert_eq!(toml::from_str::<Config>(&merge(&f, &e)).unwrap(), empty);
    }
}
