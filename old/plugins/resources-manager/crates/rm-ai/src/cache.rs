//! 提示词指纹缓存（`docs/01` §7.8.5）。
//!
//! 同一 `prompt_hash` 的请求直接复用结果，不重复调模型。

use std::collections::HashMap;

use crate::provider::RawSuggestion;

#[derive(Default)]
pub struct PromptCache {
    map: HashMap<String, Vec<RawSuggestion>>,
}

impl PromptCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, prompt_hash: &str) -> Option<&Vec<RawSuggestion>> {
        self.map.get(prompt_hash)
    }

    pub fn put(&mut self, prompt_hash: String, suggs: Vec<RawSuggestion>) {
        self.map.insert(prompt_hash, suggs);
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cache_hit_miss() {
        let mut c = PromptCache::new();
        assert!(c.get("ph_x").is_none());
        c.put(
            "ph_x".into(),
            vec![RawSuggestion {
                kind: "tag".into(),
                payload: json!({"tag_name": "科幻"}),
                confidence: 0.9,
            }],
        );
        assert_eq!(c.get("ph_x").map(|v| v.len()), Some(1));
    }
}
