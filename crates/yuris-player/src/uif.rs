//! Translation patches made with AtomCrafty's UniversalInjectorFramework
//! (a `winmm.dll` proxy next to the game exe, configured by
//! `uif_config.json`).
//!
//! Its `character_substitution` keeps the scripts in Shift-JIS and swaps
//! characters when the text is drawn: patches for Chinese write a hanzi
//! Shift-JIS lacks as a spare kanji that it has, and the table maps that
//! kanji back. The mapping is one to one, pairing `source_characters` and
//! `target_characters` by position (UIF stops at the shorter one). Other
//! keys such as `remain`/`repeat` are bookkeeping of the patch tools; UIF
//! itself ignores them.

use std::collections::HashMap;

/// The substitution table of a `uif_config.json`, when the feature is on.
pub fn character_substitutions(json: &[u8]) -> Option<HashMap<char, char>> {
    let config: serde_json::Value = serde_json::from_slice(json).ok()?;
    let feature = config.get("character_substitution")?;
    if feature.get("enable").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let chars = |key: &str| feature.get(key).and_then(serde_json::Value::as_str).unwrap_or("").chars();
    let map: HashMap<char, char> = chars("source_characters").zip(chars("target_characters")).collect();
    (!map.is_empty()).then_some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_by_position_only_when_enabled() {
        let on = r#"{"character_substitution":{"enable":true,"source_characters":"堯業輩","target_characters":"啊业","remain":["唎"]}}"#;
        let map = character_substitutions(on.as_bytes()).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map[&'堯'], '啊');
        assert_eq!(map[&'業'], '业');
        let off = on.replace("true", "false");
        assert!(character_substitutions(off.as_bytes()).is_none());
        assert!(character_substitutions(b"{}").is_none());
    }
}
