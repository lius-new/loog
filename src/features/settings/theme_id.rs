//! Pure stable serialized theme identifiers.
//!
//! Built-in themes come first and never move. Themes that extension packages
//! contribute are added after them and keep their place for the rest of the run,
//! even while their package is stopped or removed: a selection is an index, and a
//! theme that is gone just stops being offered and draws as the default.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::{Mutex, RwLock};

const IDS: &[&str] = &["aura-dark"];

struct Contributed {
    id: &'static str,
    available: bool,
}

static CONTRIBUTED: RwLock<Vec<Contributed>> = RwLock::new(Vec::new());

/// A theme id that settings named before any package could provide it. Settings
/// are read before packages load, so the choice is kept until it can be honored.
static WANTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Unknown ids kept at most; settings name one, a damaged file could name many.
const MAX_WANTED: usize = 8;

/// The registry is process-wide; tests that change it take turns.
#[cfg(test)]
pub(crate) static REGISTRY_TURN: Mutex<()> = Mutex::new(());

/// A theme: a built-in one or one an extension contributes. Stored in
/// `settings.json` as the theme's string id; an unknown id loads as the default
/// theme (and is remembered, see [`ThemeId::take_wanted`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThemeId(usize); // index: built-in themes, then contributed ones; 0 is the default

impl ThemeId {
    /// The themes that can be chosen now.
    pub fn all() -> impl Iterator<Item = Self> {
        let contributed = CONTRIBUTED.read().unwrap();
        let mut ids: Vec<Self> = (0..IDS.len()).map(Self).collect();
        ids.extend(
            contributed
                .iter()
                .enumerate()
                .filter(|(_, theme)| theme.available)
                .map(|(at, _)| Self(IDS.len() + at)),
        );
        ids.into_iter()
    }

    pub fn parse(id: &str) -> Option<Self> {
        if let Some(at) = IDS.iter().position(|name| *name == id) {
            return Some(Self(at));
        }
        CONTRIBUTED
            .read()
            .unwrap()
            .iter()
            .position(|theme| theme.available && theme.id == id)
            .map(|at| Self(IDS.len() + at))
    }

    pub fn id(self) -> &'static str {
        match self.0.checked_sub(IDS.len()) {
            None => IDS[self.0],
            Some(at) => CONTRIBUTED.read().unwrap().get(at).map_or(IDS[0], |t| t.id),
        }
    }

    /// Whether this theme can be drawn now (a contributed one may have gone).
    pub fn is_available(self) -> bool {
        match self.0.checked_sub(IDS.len()) {
            None => true,
            Some(at) => CONTRIBUTED
                .read()
                .unwrap()
                .get(at)
                .is_some_and(|t| t.available),
        }
    }

    pub(crate) fn index(self) -> usize {
        self.0
    }
    pub(crate) fn from_index(index: usize) -> Self {
        Self(index)
    }

    /// Declares which contributed themes exist now, by id. A theme seen before
    /// keeps its index; one that is not listed stops being available. Returns the
    /// index of each id, in the order given.
    pub fn set_contributed(ids: &[&str]) -> Vec<usize> {
        let mut contributed = CONTRIBUTED.write().unwrap();
        for theme in contributed.iter_mut() {
            theme.available = ids.contains(&theme.id);
        }
        ids.iter()
            .map(|id| {
                let at = match contributed.iter().position(|theme| theme.id == *id) {
                    Some(at) => at,
                    None => {
                        // Ids are few and live for the run, like the built-in ones.
                        let id: &'static str = Box::leak((*id).to_owned().into_boxed_str());
                        contributed.push(Contributed {
                            id,
                            available: true,
                        });
                        contributed.len() - 1
                    }
                };
                IDS.len() + at
            })
            .collect()
    }

    /// The theme settings asked for before it existed, once it does. Settings
    /// that named it are then changed to it.
    pub fn take_wanted() -> Option<Self> {
        let mut wanted = WANTED.lock().unwrap();
        let (at, found) = wanted
            .iter()
            .enumerate()
            .find_map(|(at, id)| Self::parse(id).map(|found| (at, found)))?;
        wanted.remove(at);
        Some(found)
    }
}

impl Serialize for ThemeId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.id())
    }
}

impl<'de> Deserialize<'de> for ThemeId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        Ok(Self::parse(&id).unwrap_or_else(|| {
            // Perhaps a package theme that has not loaded yet.
            let mut wanted = WANTED.lock().unwrap();
            if wanted.len() < MAX_WANTED && !wanted.contains(&id) {
                wanted.push(id);
            }
            Self::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::REGISTRY_TURN as TURN;

    #[test]
    fn contributed_themes_keep_their_place_and_go_away_without_moving_others() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        let first = ThemeId::set_contributed(&["test.pkg.night", "test.pkg.dawn"]);
        assert_eq!(first[0] + 1, first[1]);
        let night = ThemeId::parse("test.pkg.night").unwrap();
        assert_eq!(night.index(), first[0]);
        assert!(ThemeId::all().any(|id| id == night));

        // One goes; the other keeps its index; the gone one is not offered.
        let again = ThemeId::set_contributed(&["test.pkg.dawn"]);
        assert_eq!(again[0], first[1]);
        assert!(ThemeId::parse("test.pkg.night").is_none());
        assert!(!night.is_available());
        assert!(!ThemeId::all().any(|id| id == night));
        assert_eq!(
            night.id(),
            "test.pkg.night",
            "a selection can still name it"
        );

        // It comes back at the same place.
        let back = ThemeId::set_contributed(&["test.pkg.dawn", "test.pkg.night"]);
        assert_eq!(back[1], first[0]);
        assert!(night.is_available());
        ThemeId::set_contributed(&[]);
    }

    #[test]
    fn an_unknown_id_is_remembered_until_a_package_provides_it() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        let loaded: ThemeId = serde_json::from_str("\"test.late.theme\"").unwrap();
        assert_eq!(loaded, ThemeId::default());
        ThemeId::set_contributed(&["test.late.theme"]);
        let found = ThemeId::take_wanted().expect("the theme exists now");
        assert_eq!(found.id(), "test.late.theme");
        assert!(
            !WANTED
                .lock()
                .unwrap()
                .iter()
                .any(|id| id == "test.late.theme"),
            "taken once"
        );
        ThemeId::set_contributed(&[]);
    }
}
