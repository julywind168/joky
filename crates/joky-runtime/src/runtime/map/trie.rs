//! Immutable bitmap-indexed trie. Only entries own Joky payload references.
use super::super::hashing::KeyOps;
use super::super::managed::drop_list_words;
use std::sync::Arc;

pub(super) struct Entry {
    pub hash: u64,
    pub key_words: usize,
    pub ops: KeyOps,
    pub words: Box<[u64]>,
    pub masks: Box<[u64]>,
}

impl Entry {
    fn matches(&self, key: &[u64], ops: KeyOps, hash: u64) -> bool {
        self.key_words == key.len()
            && (!self.ops.hashed() || !ops.hashed() || self.hash == hash)
            && ops.equal(&self.words[..self.key_words], &self.masks, key)
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        drop_list_words(&self.words, &self.masks);
    }
}

pub(super) struct Node {
    pub length: usize,
    pub kind: Kind,
}

pub(super) enum Kind {
    Leaf(Arc<Entry>),
    Branch {
        bitmap: u32,
        children: Box<[Arc<Node>]>,
    },
    Collision {
        hash: u64,
        entries: Box<[Arc<Entry>]>,
    },
}

fn leaf(entry: Arc<Entry>) -> Arc<Node> {
    Arc::new(Node {
        length: 1,
        kind: Kind::Leaf(entry),
    })
}

fn branch(bitmap: u32, children: Vec<Arc<Node>>) -> Arc<Node> {
    Arc::new(Node {
        length: children.iter().map(|child| child.length).sum(),
        kind: Kind::Branch {
            bitmap,
            children: children.into_boxed_slice(),
        },
    })
}

fn collision(hash: u64, entries: Vec<Arc<Entry>>) -> Arc<Node> {
    debug_assert!(entries.len() >= 2);
    Arc::new(Node {
        length: entries.len(),
        kind: Kind::Collision {
            hash,
            entries: entries.into_boxed_slice(),
        },
    })
}

fn bit(hash: u64, shift: u32) -> u32 {
    debug_assert!(shift < 64);
    1 << ((hash >> shift) & 31)
}

fn index(bitmap: u32, bit: u32) -> usize {
    (bitmap & (bit - 1)).count_ones() as usize
}

// Different hashes must diverge by the final (60-bit offset) level.
fn join(
    left: Arc<Node>,
    left_hash: u64,
    right: Arc<Node>,
    right_hash: u64,
    shift: u32,
) -> Arc<Node> {
    debug_assert_ne!(left_hash, right_hash);
    let a = bit(left_hash, shift);
    let b = bit(right_hash, shift);
    if a == b {
        branch(a, vec![join(left, left_hash, right, right_hash, shift + 5)])
    } else if a < b {
        branch(a | b, vec![left, right])
    } else {
        branch(a | b, vec![right, left])
    }
}

pub(super) fn find<'a>(
    node: &'a Node,
    key: &[u64],
    ops: KeyOps,
    hash: u64,
    indexed: bool,
    shift: u32,
) -> Option<&'a Arc<Entry>> {
    match &node.kind {
        Kind::Leaf(entry) => entry.matches(key, ops, hash).then_some(entry),
        Kind::Collision {
            hash: stored,
            entries,
        } => {
            if indexed && *stored != hash {
                return None;
            }
            entries.iter().find(|entry| entry.matches(key, ops, hash))
        }
        Kind::Branch { bitmap, children } => {
            if !indexed {
                return children
                    .iter()
                    .find_map(|child| find(child, key, ops, hash, false, shift + 5));
            }
            let bit = bit(hash, shift);
            if bitmap & bit == 0 {
                return None;
            }
            find(
                &children[index(*bitmap, bit)],
                key,
                ops,
                hash,
                true,
                shift + 5,
            )
        }
    }
}

pub(super) fn insert(
    node: Option<&Arc<Node>>,
    entry: Arc<Entry>,
    replaced: Option<&Arc<Entry>>,
    shift: u32,
) -> Arc<Node> {
    let Some(node) = node else {
        return leaf(entry);
    };
    match &node.kind {
        Kind::Leaf(old) => {
            if replaced.is_some_and(|replaced| Arc::ptr_eq(replaced, old)) {
                leaf(entry)
            } else if old.hash == entry.hash {
                collision(entry.hash, vec![old.clone(), entry])
            } else {
                let hash = entry.hash;
                join(node.clone(), old.hash, leaf(entry), hash, shift)
            }
        }
        Kind::Collision { hash, entries } => {
            if *hash != entry.hash {
                let new_hash = entry.hash;
                return join(node.clone(), *hash, leaf(entry), new_hash, shift);
            }
            let mut entries = entries.to_vec();
            if let Some(index) =
                replaced.and_then(|old| entries.iter().position(|entry| Arc::ptr_eq(old, entry)))
            {
                entries[index] = entry;
            } else {
                entries.push(entry);
            }
            collision(*hash, entries)
        }
        Kind::Branch { bitmap, children } => {
            let bit = bit(entry.hash, shift);
            let index = index(*bitmap, bit);
            let mut children = children.to_vec();
            if bitmap & bit == 0 {
                children.insert(index, leaf(entry));
            } else {
                children[index] = insert(Some(&children[index]), entry, replaced, shift + 5);
            }
            branch(bitmap | bit, children)
        }
    }
}

pub(super) fn remove(node: &Arc<Node>, entry: &Arc<Entry>, shift: u32) -> Option<Arc<Node>> {
    match &node.kind {
        Kind::Leaf(old) => (!Arc::ptr_eq(old, entry)).then(|| node.clone()),
        Kind::Collision { hash, entries } => {
            let entries = entries
                .iter()
                .filter(|old| !Arc::ptr_eq(old, entry))
                .cloned()
                .collect::<Vec<_>>();
            match entries.len() {
                0 => None,
                1 => Some(leaf(entries[0].clone())),
                _ => Some(collision(*hash, entries)),
            }
        }
        Kind::Branch { bitmap, children } => {
            let bit = bit(entry.hash, shift);
            if bitmap & bit == 0 {
                return Some(node.clone());
            }
            let index = index(*bitmap, bit);
            let mut children = children.to_vec();
            let mut bitmap = *bitmap;
            if let Some(child) = remove(&children[index], entry, shift + 5) {
                children[index] = child;
            } else {
                children.remove(index);
                bitmap &= !bit;
            }
            match children.len() {
                0 => None,
                // A branch encodes its depth implicitly, so only terminals can move up.
                1 if !matches!(children[0].kind, Kind::Branch { .. }) => Some(children[0].clone()),
                _ => Some(branch(bitmap, children)),
            }
        }
    }
}
