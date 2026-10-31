//! Extension settings (design §6): a namespaced schema, user and project
//! layers, and change detection.
//!
//! Project files can only supply data for settings the extension declared and
//! allows at project scope; they can never add a permission, and in an
//! untrusted root they are not used at all.
use super::manifest::{SettingDecl, SettingKind, SettingScope};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn matches_kind(value: &Value, kind: SettingKind) -> bool {
    match kind {
        SettingKind::String => value.is_string(),
        SettingKind::Number => value.is_number(),
        SettingKind::Boolean => value.is_boolean(),
    }
}

/// A value from a settings file that could not be used, for diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ignored {
    pub key: String,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resolved {
    /// JSON text per key, the form the guest reads.
    pub values: BTreeMap<String, String>,
    pub ignored: Vec<Ignored>,
}

/// Resolve every declared key: default, then the user value, then the project
/// value (project-scoped settings in a trusted root only).
pub fn resolve(
    decls: &[SettingDecl],
    user: &Value,
    project: Option<&Value>,
    root_trusted: bool,
) -> Resolved {
    let mut resolved = Resolved::default();
    for decl in decls {
        let mut value = decl.default.clone();
        for (layer, source) in [("user", Some(user)), ("project", project)] {
            let Some(found) = source.and_then(|s| s.get(&decl.key)) else {
                continue;
            };
            if layer == "project" {
                if decl.scope == SettingScope::User {
                    resolved.ignored.push(Ignored {
                        key: decl.key.clone(),
                        reason: "this setting can only be set by the user",
                    });
                    continue;
                }
                if !root_trusted {
                    resolved.ignored.push(Ignored {
                        key: decl.key.clone(),
                        reason: "the folder is not trusted",
                    });
                    continue;
                }
            }
            if matches_kind(found, decl.kind) {
                value = found.clone();
            } else {
                resolved.ignored.push(Ignored {
                    key: decl.key.clone(),
                    reason: "the value has the wrong type",
                });
            }
        }
        resolved.values.insert(decl.key.clone(), value.to_string());
    }
    for source in [Some(user), project].into_iter().flatten() {
        if let Some(object) = source.as_object() {
            for key in object.keys() {
                if !decls.iter().any(|d| &d.key == key)
                    && !resolved.ignored.iter().any(|i| &i.key == key)
                {
                    resolved.ignored.push(Ignored {
                        key: key.clone(),
                        reason: "no extension declares this setting",
                    });
                }
            }
        }
    }
    resolved
}

/// Keys whose resolved value differs between two resolutions.
pub fn changed(old: &BTreeMap<String, String>, new: &BTreeMap<String, String>) -> Vec<String> {
    new.iter()
        .filter(|(key, value)| old.get(*key) != Some(*value))
        .map(|(key, _)| key.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decls() -> Vec<SettingDecl> {
        vec![
            SettingDecl {
                key: "example.tools.width".into(),
                kind: SettingKind::Number,
                default: json!(80),
                scope: SettingScope::Project,
            },
            SettingDecl {
                key: "example.tools.token".into(),
                kind: SettingKind::String,
                default: json!(""),
                scope: SettingScope::User,
            },
        ]
    }

    #[test]
    fn layers_apply_in_order_default_user_project() {
        let user = json!({ "example.tools.width": 100 });
        let project = json!({ "example.tools.width": 120 });
        assert_eq!(
            resolve(&decls(), &json!({}), None, true).values["example.tools.width"],
            "80"
        );
        assert_eq!(
            resolve(&decls(), &user, None, true).values["example.tools.width"],
            "100"
        );
        assert_eq!(
            resolve(&decls(), &user, Some(&project), true).values["example.tools.width"],
            "120"
        );
    }

    #[test]
    fn project_files_cannot_set_user_scoped_settings_or_act_in_untrusted_roots() {
        let project = json!({ "example.tools.token": "stolen", "example.tools.width": 5 });
        let trusted = resolve(&decls(), &json!({}), Some(&project), true);
        assert_eq!(trusted.values["example.tools.token"], "\"\"");
        assert!(trusted.ignored.iter().any(
            |i| i.key == "example.tools.token" && i.reason.contains("only be set by the user")
        ));
        let untrusted = resolve(&decls(), &json!({}), Some(&project), false);
        assert_eq!(untrusted.values["example.tools.width"], "80");
        assert!(
            untrusted
                .ignored
                .iter()
                .any(|i| i.reason.contains("not trusted"))
        );
    }

    #[test]
    fn wrong_types_and_undeclared_keys_are_reported_not_used() {
        let user = json!({ "example.tools.width": "wide", "other.ext.thing": 1 });
        let resolved = resolve(&decls(), &user, None, true);
        assert_eq!(resolved.values["example.tools.width"], "80");
        assert!(
            resolved
                .ignored
                .iter()
                .any(|i| i.reason.contains("wrong type"))
        );
        assert!(resolved.ignored.iter().any(|i| i.key == "other.ext.thing"));
    }

    #[test]
    fn change_detection_names_only_the_keys_that_differ() {
        let a = resolve(&decls(), &json!({}), None, true).values;
        let b = resolve(&decls(), &json!({ "example.tools.width": 90 }), None, true).values;
        assert_eq!(changed(&a, &b), ["example.tools.width"]);
        assert!(changed(&a, &a).is_empty());
    }
}
