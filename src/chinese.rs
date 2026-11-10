//! Generic Chinese script conversion using the pinned OpenCC dictionaries.
use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChineseScript {
    #[default]
    Simplified,
    Traditional,
}

#[derive(Default)]
struct Node {
    children: HashMap<char, Node>,
    value: Option<&'static str>,
}

impl Node {
    fn load(tables: &[&'static str]) -> Self {
        let mut root = Self::default();
        for table in tables {
            for line in table.lines().filter(|line| !line.starts_with('#')) {
                let Some((key, values)) = line.split_once('\t') else {
                    continue;
                };
                let Some(value) = values.split_whitespace().next() else {
                    continue;
                };
                let mut node = &mut root;
                for character in key.chars() {
                    node = node.children.entry(character).or_default();
                }
                node.value = Some(value);
            }
        }
        root
    }
}

impl ChineseScript {
    pub fn convert(self, text: &str) -> String {
        static SIMPLIFIED: OnceLock<Node> = OnceLock::new();
        static TRADITIONAL: OnceLock<Node> = OnceLock::new();
        let root = match self {
            Self::Simplified => SIMPLIFIED.get_or_init(|| {
                Node::load(&[
                    include_str!("../data/chinese_conversion/TSCharacters.txt"),
                    include_str!("../data/chinese_conversion/TSPhrases.txt"),
                ])
            }),
            Self::Traditional => TRADITIONAL.get_or_init(|| {
                Node::load(&[
                    include_str!("../data/chinese_conversion/STCharacters.txt"),
                    include_str!("../data/chinese_conversion/STPhrases.txt"),
                ])
            }),
        };
        let mut output = String::with_capacity(text.len());
        let mut offset = 0;
        while offset < text.len() {
            let mut node = root;
            let mut longest = None;
            for (index, character) in text[offset..].char_indices() {
                let Some(child) = node.children.get(&character) else {
                    break;
                };
                node = child;
                if let Some(value) = node.value {
                    longest = Some((index + character.len_utf8(), value));
                }
            }
            if let Some((length, value)) = longest {
                output.push_str(value);
                offset += length;
            } else {
                let character = text[offset..].chars().next().unwrap();
                output.push(character);
                offset += character.len_utf8();
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chinese_conversion_longest_prefix() {
        let source = "头发 发展 中国 软件 abc😀，";
        let traditional = "頭髮 發展 中國 軟件 abc😀，";
        assert_eq!(ChineseScript::Traditional.convert(source), traditional);
        assert_eq!(ChineseScript::Simplified.convert(traditional), source);
    }
}
