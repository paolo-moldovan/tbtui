//! Regex filters for the tag and run trees. A filter is either typed as a
//! regex or generated with grex from example names.

use crate::session::FilterSaved;
use grex::RegExpBuilder;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GrexOpts {
    /// \d for digits: "seed0", "seed1" also match "seed7"
    pub digits: bool,
    /// \w for word characters
    pub words: bool,
    /// {n} for repeated substrings
    pub repetitions: bool,
    /// ^…$ (whole name) vs. anywhere in the name
    pub anchors: bool,
}

impl Default for GrexOpts {
    fn default() -> Self {
        GrexOpts { digits: true, words: false, repetitions: false, anchors: true }
    }
}

#[derive(Clone, Default)]
pub struct Filter {
    pub text: String,
    pub examples: Vec<String>,
    pub grex: GrexOpts,
    re: Option<Regex>,
    /// text is not a valid regex (it is then matched literally)
    pub invalid: bool,
}

impl Filter {
    pub fn new(text: &str) -> Self {
        let mut f = Filter::default();
        f.set_text(text);
        f
    }

    pub fn to_saved(&self) -> FilterSaved {
        FilterSaved { text: self.text.clone(), examples: self.examples.clone(), grex: self.grex }
    }

    pub fn from_saved(s: &FilterSaved) -> Self {
        let mut f = Filter { examples: s.examples.clone(), grex: s.grex, ..Default::default() };
        f.set_text(&s.text);
        f
    }

    pub fn is_active(&self) -> bool {
        !self.text.is_empty()
    }

    pub fn matches(&self, s: &str) -> bool {
        self.re.as_ref().is_none_or(|r| r.is_match(s))
    }

    pub fn set_text(&mut self, t: &str) {
        self.text = t.to_string();
        self.invalid = false;
        self.re = if t.is_empty() {
            None
        } else {
            match RegexBuilder::new(t).case_insensitive(true).build() {
                Ok(r) => Some(r),
                Err(_) => {
                    self.invalid = true;
                    RegexBuilder::new(&regex::escape(t)).case_insensitive(true).build().ok()
                }
            }
        };
    }

    pub fn is_example(&self, s: &str) -> bool {
        self.examples.iter().any(|e| e == s)
    }

    /// Add the names as examples, or remove them if they all already are.
    pub fn toggle_examples(&mut self, names: &[String]) {
        if !names.is_empty() && names.iter().all(|n| self.is_example(n)) {
            self.examples.retain(|e| !names.contains(e));
        } else {
            for n in names {
                if !self.is_example(n) {
                    self.examples.push(n.clone());
                }
            }
        }
        self.regenerate();
    }

    pub fn add_example(&mut self, s: &str) {
        if !s.is_empty() && !self.is_example(s) {
            self.examples.push(s.to_string());
        }
        self.regenerate();
    }

    pub fn clear(&mut self) {
        self.examples.clear();
        self.set_text("");
    }

    /// Rebuild the regex from the examples with grex.
    pub fn regenerate(&mut self) {
        if self.examples.is_empty() {
            self.set_text("");
            return;
        }
        let mut b = RegExpBuilder::from(&self.examples);
        if self.grex.digits {
            b.with_conversion_of_digits();
        }
        if self.grex.words {
            b.with_conversion_of_words();
        }
        if self.grex.repetitions {
            b.with_conversion_of_repetitions();
        }
        if !self.grex.anchors {
            b.without_anchors();
        }
        let re = b.build();
        self.set_text(&re);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grex_generalizes_digits() {
        let mut f = Filter::default();
        f.toggle_examples(&["lr_1e-3/seed0".into(), "lr_1e-3/seed1".into()]);
        assert!(f.matches("lr_1e-3/seed0"));
        assert!(f.matches("lr_1e-3/seed7"), "regex was {}", f.text);
        assert!(!f.matches("lr_3e-4"));
        // toggling the same examples again removes them
        f.toggle_examples(&["lr_1e-3/seed0".into(), "lr_1e-3/seed1".into()]);
        assert!(!f.is_active());
    }

    #[test]
    fn invalid_regex_is_literal() {
        let f = Filter::new("loss(");
        assert!(f.invalid);
        assert!(f.matches("val/loss(x)"));
        assert!(!f.matches("loss"));
    }
}
