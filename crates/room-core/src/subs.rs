//! Pattern subscriptions with coalesced pending sets (ROOM.md "Subscriptions").
//!
//! A subscription holds at most one pending value per key, so a slow subscriber
//! never builds a backlog. `Ready` is reported once each time the pending set goes
//! from empty to non-empty.

use std::collections::{BTreeMap, VecDeque};

use crate::api::{ClientId, Update};
use crate::state::Vis;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Seg {
    Lit(String),
    One,
    Rest,
}

/// `/`-separated segments; `*` matches exactly one segment; a trailing `**`
/// matches one or more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern(Vec<Seg>);

impl Pattern {
    pub fn parse(s: &str) -> Option<Pattern> {
        if s.is_empty() {
            return None;
        }
        let parts: Vec<&str> = s.split('/').collect();
        let mut segs = Vec::new();
        for (i, p) in parts.iter().enumerate() {
            segs.push(match *p {
                "" => return None,
                "*" => Seg::One,
                "**" if i + 1 == parts.len() => Seg::Rest,
                "**" => return None,
                lit => {
                    if !lit.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)
                    }) {
                        return None;
                    }
                    Seg::Lit(lit.to_string())
                }
            });
        }
        Some(Pattern(segs))
    }

    pub fn matches(&self, path: &str) -> bool {
        let mut it = path.split('/');
        for seg in &self.0 {
            match seg {
                Seg::Rest => return it.next().is_some(),
                Seg::One => {
                    if it.next().is_none() {
                        return false;
                    }
                }
                Seg::Lit(l) => {
                    if it.next() != Some(l.as_str()) {
                        return false;
                    }
                }
            }
        }
        it.next().is_none()
    }
}

fn value_update(path: &str, v: &Vis) -> Update {
    Update::Value {
        path: path.to_string(),
        writer: v.writer,
        version: v.version,
        class: v.class,
        value: v.value.clone(),
    }
}

struct Sub {
    pat: Pattern,
    /// Reset, snapshot values and SnapshotDone, in order. Delivered before `pending`.
    head: VecDeque<Update>,
    pending: BTreeMap<String, Update>,
    /// `Ready` was reported and the sub hasn't been drained empty since.
    ready: bool,
}

impl Sub {
    fn restart(&mut self, visible: &BTreeMap<String, Vis>) {
        self.pending.clear();
        self.head.clear();
        self.head.push_back(Update::Reset);
        for (p, v) in visible {
            if self.pat.matches(p) {
                self.head.push_back(value_update(p, v));
            }
        }
        self.head.push_back(Update::SnapshotDone);
    }
}

#[derive(Default)]
pub struct Subs {
    subs: BTreeMap<(ClientId, u32), Sub>,
}

impl Subs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.subs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }

    /// Start (or replace) a subscription. Returns the `Ready` edges to emit.
    pub fn subscribe(
        &mut self,
        client: ClientId,
        sub: u32,
        pat: Pattern,
        visible: &BTreeMap<String, Vis>,
    ) -> Vec<(ClientId, u32)> {
        let mut s = Sub {
            pat,
            head: VecDeque::new(),
            pending: BTreeMap::new(),
            ready: true,
        };
        s.restart(visible);
        self.subs.insert((client, sub), s);
        vec![(client, sub)]
    }

    pub fn unsubscribe(&mut self, client: ClientId, sub: u32) {
        self.subs.remove(&(client, sub));
    }

    pub fn drop_client(&mut self, client: ClientId) {
        self.subs.retain(|(c, _), _| *c != client);
    }

    /// A visible key changed (`None` = no longer visible).
    pub fn note(&mut self, path: &str, v: Option<&Vis>) -> Vec<(ClientId, u32)> {
        let mut edges = Vec::new();
        for (key, s) in self.subs.iter_mut() {
            if !s.pat.matches(path) {
                continue;
            }
            let upd = match v {
                Some(v) => value_update(path, v),
                None => Update::Removed {
                    path: path.to_string(),
                },
            };
            s.pending.insert(path.to_string(), upd);
            if !s.ready {
                s.ready = true;
                edges.push(*key);
            }
        }
        edges
    }

    /// Restart every subscription: `Reset`, the current matches, `SnapshotDone`.
    pub fn reset_all(&mut self, visible: &BTreeMap<String, Vis>) -> Vec<(ClientId, u32)> {
        let mut edges = Vec::new();
        for (key, s) in self.subs.iter_mut() {
            s.restart(visible);
            if !s.ready {
                s.ready = true;
                edges.push(*key);
            }
        }
        edges
    }

    pub fn drain(&mut self, client: ClientId, sub: u32, max: usize) -> Vec<Update> {
        let Some(s) = self.subs.get_mut(&(client, sub)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while out.len() < max {
            if let Some(u) = s.head.pop_front() {
                out.push(u);
                continue;
            }
            match s.pending.pop_first() {
                Some((_, u)) => out.push(u),
                None => break,
            }
        }
        if s.head.is_empty() && s.pending.is_empty() {
            s.ready = false;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        let p = |s| Pattern::parse(s).unwrap();
        assert!(p("member/*/pose").matches("member/ab/pose"));
        assert!(!p("member/*/pose").matches("member/ab/pose/x"));
        assert!(!p("member/*/pose").matches("member/ab"));
        assert!(p("room/**").matches("room/a"));
        assert!(p("room/**").matches("room/a/b"));
        assert!(!p("room/**").matches("room"));
        assert!(p("**").matches("x"));
        assert!(Pattern::parse("a/**/b").is_none());
        assert!(Pattern::parse("a//b").is_none());
        assert!(Pattern::parse("A").is_none());
    }
}
