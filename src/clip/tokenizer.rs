//! CLIP's byte-level BPE tokenizer in plain Rust, reading the vocabulary and merges from the
//! Hugging Face `tokenizer.json` that ships with the model. Same steps as the original:
//! NFC, collapse whitespace, lower-case, split into words / single digits / punctuation runs,
//! map bytes to printable characters, merge pairs by rank, `</w>` marks the end of a word.

use std::collections::HashMap;
use std::path::Path;

use regex::Regex;
use unicode_normalization::UnicodeNormalization;

pub const START: u32 = 49406;
pub const END: u32 = 49407;
/// CLIP's text encoder always sees 77 positions.
pub const CONTEXT: usize = 77;

pub struct Tokenizer {
    vocab: HashMap<String, u32>,
    ranks: HashMap<(String, String), u32>,
    split: Regex,
    /// Byte -> printable character (GPT-2's byte-level alphabet).
    bytes: [char; 256],
}

impl Tokenizer {
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_json(&raw)
    }

    pub fn from_json(raw: &[u8]) -> Result<Self, String> {
        #[derive(serde::Deserialize)]
        struct File {
            model: Model,
        }
        #[derive(serde::Deserialize)]
        struct Model {
            vocab: HashMap<String, u32>,
            merges: Vec<Merge>,
        }
        // Older files store merges as "a b", newer ones as ["a", "b"].
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Merge {
            Joined(String),
            Pair(String, String),
        }
        let file: File = serde_json::from_slice(raw).map_err(|e| format!("tokenizer.json: {e}"))?;
        let mut ranks = HashMap::with_capacity(file.model.merges.len());
        for (rank, m) in file.model.merges.into_iter().enumerate() {
            let pair = match m {
                Merge::Pair(a, b) => (a, b),
                Merge::Joined(s) => {
                    let (a, b) = s.split_once(' ').ok_or("tokenizer.json: bad merge")?;
                    (a.to_string(), b.to_string())
                }
            };
            ranks.insert(pair, rank as u32);
        }
        Ok(Self {
            vocab: file.model.vocab,
            ranks,
            split: Regex::new(r"'s|'t|'re|'ve|'m|'ll|'d|[\p{L}]+|[\p{N}]|[^\s\p{L}\p{N}]+").unwrap(),
            bytes: byte_alphabet(),
        })
    }

    /// Token ids with the start and end markers, cut to 77.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let text: String = text.nfc().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        let mut ids = vec![START];
        for word in self.split.find_iter(&text) {
            let mut symbols: Vec<String> = word.as_str().bytes().map(|b| self.bytes[b as usize].to_string()).collect();
            if let Some(last) = symbols.last_mut() {
                last.push_str("</w>");
            }
            self.merge(&mut symbols);
            ids.extend(symbols.iter().map(|s| self.vocab.get(s).copied().unwrap_or(END)));
        }
        ids.truncate(CONTEXT - 1);
        ids.push(END);
        ids
    }

    /// Repeatedly joins the neighbouring pair with the lowest merge rank.
    fn merge(&self, symbols: &mut Vec<String>) {
        while symbols.len() > 1 {
            let best = symbols
                .windows(2)
                .enumerate()
                .filter_map(|(i, w)| self.ranks.get(&(w[0].clone(), w[1].clone())).map(|&r| (r, i)))
                .min();
            let Some((_, i)) = best else { break };
            let next = symbols.remove(i + 1);
            symbols[i].push_str(&next);
        }
    }
}

/// GPT-2's reversible byte -> unicode table: printable Latin-1 bytes map to themselves, the
/// rest to code points from 256 up, in byte order.
fn byte_alphabet() -> [char; 256] {
    let printable = |b: u32| (33..=126).contains(&b) || (161..=172).contains(&b) || (174..=255).contains(&b);
    let mut table = ['\0'; 256];
    let mut extra = 256;
    for b in 0..256u32 {
        table[b as usize] = if printable(b) {
            char::from_u32(b).unwrap()
        } else {
            extra += 1;
            char::from_u32(extra - 1).unwrap()
        };
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokenizer() -> Option<Tokenizer> {
        let path = crate::clip::text_files()?.1;
        Some(Tokenizer::from_file(&path).expect("tokenizer.json"))
    }

    #[test]
    fn byte_alphabet_is_gpt2s() {
        let t = byte_alphabet();
        assert_eq!(t[b'a' as usize], 'a');
        assert_eq!(t[b' ' as usize], 'Ġ');
        assert_eq!(t[b'\n' as usize], 'Ċ');
        assert_eq!(t[0], 'Ā');
        let mut seen: Vec<char> = t.to_vec();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 256);
    }

    #[test]
    fn merges_by_rank_on_a_toy_vocabulary() {
        let json = br#"{"model": {"vocab": {"a": 0, "b": 1, "c</w>": 2, "ab": 3, "abc</w>": 4, "b</w>": 5},
                         "merges": ["a b", ["ab", "c</w>"]]}}"#;
        let t = Tokenizer::from_json(json).unwrap();
        assert_eq!(t.encode("abc"), vec![START, 4, END]);
        // "b</w>" is a different symbol from "b", so "a b" does not merge at the end of a word.
        assert_eq!(t.encode("  AB \n b "), vec![START, 0, 5, 5, END]);
    }

    /// Ids checked against Hugging Face `tokenizers` with Xenova/clip-vit-base-patch32.
    #[test]
    fn matches_reference_ids() {
        let Some(t) = tokenizer() else { return };
        assert_eq!(t.encode("a photo of a dog"), vec![49406, 320, 1125, 539, 320, 1929, 49407]);
    }

    #[test]
    fn truncates_to_context() {
        let Some(t) = tokenizer() else { return };
        let ids = t.encode(&"dog ".repeat(200));
        assert_eq!(ids.len(), CONTEXT);
        assert_eq!(ids[0], START);
        assert_eq!(ids[CONTEXT - 1], END);
    }

    /// Same ids as Hugging Face `tokenizers` (a dev-dependency) on awkward input.
    #[test]
    fn matches_hugging_face() {
        let Some(t) = tokenizer() else { return };
        let path = crate::clip::text_files().unwrap().1;
        let hf = tokenizers::Tokenizer::from_file(&path).unwrap();
        for text in [
            "beach",
            "Birthday cake",
            "a RED car parked   outside\n",
            "mountains with snow!!",
            "man in a suit, 1990's style",
            "it's the dog's ball; we'll see",
            "café crème brûlée",
            "cafe\u{301} — 東京タワー 🐶🎂",
            "12345 3.14 #hashtag @user",
            "don't stop'n'go",
            "",
            "   ",
        ] {
            let want: Vec<u32> = hf.encode(text, true).unwrap().get_ids().to_vec();
            assert_eq!(t.encode(text), want, "{text:?}");
        }
    }
}
