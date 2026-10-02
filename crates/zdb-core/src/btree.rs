//! B+Tree: the ordered index everything else in ZelligeDB stands on.
//!
//! A tree maps `Vec<u8> -> Vec<u8>` in lexicographic byte order. Interior
//! nodes route; leaf nodes hold entries and are chained left-to-right so
//! range scans never touch the interior levels.
//!
//! Layout inside the 4076-byte payload region (little-endian):
//!
//! ```text
//! leaf:      kind u16 | count u16 | prev u32 | next u32
//!            then entries: (klen u16, vlen u16, key, val)*
//! interior:  kind u16 | count u16 | first_child u32
//!            then entries: (klen u16, key, child u32)*
//! ```
//!
//! Routing rule: separator `sep_i` is the smallest key in the subtree to
//! its right, so a lookup for `k` descends into `entries[i-1].child` where
//! `i` = number of separators `<= k`. Leaf splits keep the separator in the
//! right node; interior splits promote (and remove) it — the classic B+Tree
//! distinction. Every non-root node keeps >= [`MIN_ENTRIES`] entries; less
//! than that triggers borrow-from-sibling or merge on delete. See ADR-0003.

use crate::error::DbError;
use crate::io::PageIo;
use crate::page::{NIL_PAGE, PAYLOAD_LEN, Page, PageId, PageType};

const KIND_INTERIOR: usize = 0;
const KIND_LEAF: usize = 1;

const LEAF_HEADER: usize = 12; // kind + count + prev/next
const INTERIOR_HEADER: usize = 12; // kind + count + first_child

/// Minimum entry count for any non-root node. Deliberately small: it keeps
/// merge/borrow paths heavily exercised by tests on small trees. Revisit
/// with benchmark data (phase 7).
const MIN_ENTRIES: usize = 2;

const MAX_KEY_LEN: usize = 800;
const MAX_VAL_LEN: usize = 800;

const fn leaf_entry_size(key_len: usize, val_len: usize) -> usize {
    2 + 2 + key_len + val_len
}

const fn interior_entry_size(key_len: usize) -> usize {
    2 + key_len + 4
}

/// A B+Tree rooted in the database file. `root` is `None` for an empty tree.
///
/// The tree owns no pager: every method takes the pager it lives in, so any
/// number of trees can share one file. Phase 3 generalizes the parameter to
/// `&mut dyn PageIo` so the same code runs through the WAL-logging layer.
#[derive(Debug, Clone)]
pub struct BTree {
    pub root: Option<PageId>,
}

struct Split {
    sep: Vec<u8>,
    right: PageId,
}

// ---------------------------------------------------------------------------
// Node encode / decode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Leaf {
    prev: PageId,
    next: PageId,
    entries: Vec<(Vec<u8>, Vec<u8>)>,
}

#[derive(Debug, Clone)]
struct Interior {
    first_child: PageId,
    entries: Vec<(Vec<u8>, PageId)>, // (separator, right child)
}

fn leaf_bytes(entries: &[(Vec<u8>, Vec<u8>)]) -> usize {
    entries
        .iter()
        .map(|(k, v)| leaf_entry_size(k.len(), v.len()))
        .sum()
}

fn interior_bytes(entries: &[(Vec<u8>, PageId)]) -> usize {
    entries
        .iter()
        .map(|(k, _)| interior_entry_size(k.len()))
        .sum()
}

fn put_u16(buf: &mut [u8], off: usize, v: usize) {
    buf[off..off + 2].copy_from_slice(&(v as u16).to_le_bytes());
}

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn u16_at(p: &[u8], off: usize) -> usize {
    u16::from_le_bytes([p[off], p[off + 1]]) as usize
}

fn u32_at(p: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([p[off], p[off + 1], p[off + 2], p[off + 3]])
}

impl Leaf {
    fn encode(&self, payload: &mut [u8]) -> Result<(), DbError> {
        let need = LEAF_HEADER + leaf_bytes(&self.entries);
        if need > payload.len() {
            return Err(DbError::PageFull);
        }
        put_u16(payload, 0, KIND_LEAF);
        put_u16(payload, 2, self.entries.len());
        put_u32(payload, 4, self.prev);
        put_u32(payload, 8, self.next);
        let mut off = LEAF_HEADER;
        for (k, v) in &self.entries {
            put_u16(payload, off, k.len());
            put_u16(payload, off + 2, v.len());
            payload[off + 4..off + 4 + k.len()].copy_from_slice(k);
            payload[off + 4 + k.len()..off + 4 + k.len() + v.len()].copy_from_slice(v);
            off += leaf_entry_size(k.len(), v.len());
        }
        Ok(())
    }

    fn decode(payload: &[u8]) -> Result<Self, DbError> {
        if payload.len() < LEAF_HEADER || u16_at(payload, 0) != KIND_LEAF {
            return Err(DbError::Corrupt("leaf node header"));
        }
        let count = u16_at(payload, 2);
        let mut leaf = Leaf {
            prev: u32_at(payload, 4),
            next: u32_at(payload, 8),
            entries: Vec::with_capacity(count),
        };
        let mut off = LEAF_HEADER;
        for _ in 0..count {
            let klen = u16_at(payload, off);
            let vlen = u16_at(payload, off + 2);
            let end = off + leaf_entry_size(klen, vlen);
            if end > payload.len() {
                return Err(DbError::Corrupt("leaf entry exceeds page"));
            }
            leaf.entries.push((
                payload[off + 4..off + 4 + klen].to_vec(),
                payload[off + 4 + klen..end].to_vec(),
            ));
            off = end;
        }
        Ok(leaf)
    }
}

impl Interior {
    fn encode(&self, payload: &mut [u8]) -> Result<(), DbError> {
        let need = INTERIOR_HEADER + interior_bytes(&self.entries);
        if need > payload.len() {
            return Err(DbError::PageFull);
        }
        put_u16(payload, 0, KIND_INTERIOR);
        put_u16(payload, 2, self.entries.len());
        put_u32(payload, 4, self.first_child);
        let mut off = INTERIOR_HEADER;
        for (k, child) in &self.entries {
            put_u16(payload, off, k.len());
            payload[off + 2..off + 2 + k.len()].copy_from_slice(k);
            put_u32(payload, off + 2 + k.len(), *child);
            off += interior_entry_size(k.len());
        }
        Ok(())
    }

    fn decode(payload: &[u8]) -> Result<Self, DbError> {
        if payload.len() < INTERIOR_HEADER || u16_at(payload, 0) != KIND_INTERIOR {
            return Err(DbError::Corrupt("interior node header"));
        }
        let count = u16_at(payload, 2);
        let mut node = Interior {
            first_child: u32_at(payload, 4),
            entries: Vec::with_capacity(count),
        };
        let mut off = INTERIOR_HEADER;
        for _ in 0..count {
            let klen = u16_at(payload, off);
            let end = off + interior_entry_size(klen);
            if end > payload.len() {
                return Err(DbError::Corrupt("interior entry exceeds page"));
            }
            let mut child_bytes = [0u8; 4];
            child_bytes.copy_from_slice(&payload[end - 4..end]);
            node.entries.push((
                payload[off + 2..off + 2 + klen].to_vec(),
                u32::from_le_bytes(child_bytes),
            ));
            off = end;
        }
        Ok(node)
    }
}

// ---------------------------------------------------------------------------
// Node I/O through the pager
// ---------------------------------------------------------------------------

fn read_leaf(pager: &mut dyn PageIo, page_id: PageId) -> Result<Leaf, DbError> {
    let page = pager.read_page(page_id)?;
    if page.page_type()? != PageType::BTreeLeaf {
        return Err(DbError::Corrupt("page is not a B+Tree leaf"));
    }
    Leaf::decode(page.payload())
}

fn read_interior(pager: &mut dyn PageIo, page_id: PageId) -> Result<Interior, DbError> {
    let page = pager.read_page(page_id)?;
    if page.page_type()? != PageType::BTreeInterior {
        return Err(DbError::Corrupt("page is not a B+Tree interior"));
    }
    Interior::decode(page.payload())
}

fn write_leaf(pager: &mut dyn PageIo, page_id: PageId, leaf: &Leaf) -> Result<(), DbError> {
    let mut page = Page::zeroed(page_id, PageType::BTreeLeaf);
    leaf.encode(page.payload_mut())?;
    pager.write_page(&mut page)
}

fn write_interior(pager: &mut dyn PageIo, page_id: PageId, node: &Interior) -> Result<(), DbError> {
    let mut page = Page::zeroed(page_id, PageType::BTreeInterior);
    node.encode(page.payload_mut())?;
    pager.write_page(&mut page)
}

/// Child subtree holding `key`: index = number of separators `<= key`.
fn child_index(entries: &[(Vec<u8>, PageId)], key: &[u8]) -> usize {
    entries.partition_point(|(sep, _)| sep.as_slice() <= key)
}

// ---------------------------------------------------------------------------
// BTree operations
// ---------------------------------------------------------------------------

impl BTree {
    /// An empty tree. No pages are allocated until the first insert.
    pub fn empty() -> Self {
        BTree { root: None }
    }

    pub fn get(&self, pager: &mut dyn PageIo, key: &[u8]) -> Result<Option<Vec<u8>>, DbError> {
        let mut page_id = match self.root {
            Some(root) => root,
            None => return Ok(None),
        };
        let leaf = loop {
            let page = pager.read_page(page_id)?;
            match page.page_type()? {
                PageType::BTreeInterior => {
                    let node = Interior::decode(page.payload())?;
                    let i = child_index(&node.entries, key);
                    page_id = if i == 0 {
                        node.first_child
                    } else {
                        node.entries[i - 1].1
                    };
                }
                PageType::BTreeLeaf => break Leaf::decode(page.payload())?,
                _ => return Err(DbError::Corrupt("unexpected page type in tree walk")),
            }
        };
        Ok(
            match leaf
                .entries
                .binary_search_by(|(k, _)| k.as_slice().cmp(key))
            {
                Ok(i) => Some(leaf.entries[i].1.clone()),
                Err(_) => None,
            },
        )
    }

    /// Insert or overwrite a key.
    pub fn insert(
        &mut self,
        pager: &mut dyn PageIo,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), DbError> {
        if key.is_empty() {
            return Err(DbError::Corrupt("empty key"));
        }
        if key.len() > MAX_KEY_LEN || value.len() > MAX_VAL_LEN {
            return Err(DbError::EntryTooLarge {
                key_len: key.len(),
                val_len: value.len(),
                max: MAX_KEY_LEN.min(MAX_VAL_LEN),
            });
        }

        let root = match self.root {
            Some(root) => root,
            None => {
                // First insert: allocate the root leaf directly.
                let page_id = pager.alloc_page(PageType::BTreeLeaf)?.page_id();
                let leaf = Leaf {
                    prev: NIL_PAGE,
                    next: NIL_PAGE,
                    entries: vec![(key.to_vec(), value.to_vec())],
                };
                write_leaf(pager, page_id, &leaf)?;
                self.root = Some(page_id);
                return Ok(());
            }
        };

        if let Some(split) = insert_rec(pager, root, key, value)? {
            let new_root_id = pager.alloc_page(PageType::BTreeInterior)?.page_id();
            let new_root = Interior {
                first_child: root,
                entries: vec![(split.sep, split.right)],
            };
            write_interior(pager, new_root_id, &new_root)?;
            self.root = Some(new_root_id);
        }
        Ok(())
    }

    /// Delete a key. Returns whether it existed.
    pub fn delete(&mut self, pager: &mut dyn PageIo, key: &[u8]) -> Result<bool, DbError> {
        let root = match self.root {
            Some(root) => root,
            None => return Ok(false),
        };
        let (deleted, _underflow) = delete_rec(pager, root, key)?;

        // Collapse a root that shrank to a single child: the child becomes
        // the new root and the old root page is recycled.
        let page = pager.read_page(root)?;
        if page.page_type()? == PageType::BTreeInterior {
            let node = Interior::decode(page.payload())?;
            if node.entries.is_empty() {
                let child = node.first_child;
                pager.free_page(root)?;
                self.root = Some(child);
            }
        }
        Ok(deleted)
    }

    /// Full ascending scan, driven by the leaf chain.
    pub fn scan<'a>(&self, pager: &'a mut dyn PageIo) -> Result<BTreeScan<'a>, DbError> {
        self.range(pager, None, None)
    }

    /// Bounded ascending scan: yields keys with `start <= k < end`
    /// (either bound optional). Descends directly to the leaf that holds
    /// `start` — the index-scan path the SQL planner uses.
    pub fn range<'a>(
        &self,
        pager: &'a mut dyn PageIo,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<BTreeScan<'a>, DbError> {
        let Some(root) = self.root else {
            return Ok(BTreeScan {
                pager,
                leaf_id: NIL_PAGE,
                idx: 0,
                done: true,
                end: None,
            });
        };

        // Descend to the leaf that would hold `start` (or the leftmost leaf).
        let mut page_id = root;
        let leaf = loop {
            let page = pager.read_page(page_id)?;
            match page.page_type()? {
                PageType::BTreeInterior => {
                    let node = Interior::decode(page.payload())?;
                    let i = child_index(&node.entries, start.unwrap_or(b""));
                    page_id = if i == 0 {
                        node.first_child
                    } else {
                        node.entries[i - 1].1
                    };
                }
                PageType::BTreeLeaf => break Leaf::decode(page.payload())?,
                _ => return Err(DbError::Corrupt("unexpected page type in range scan")),
            }
        };

        let mut idx = 0;
        if let Some(start) = start {
            idx = leaf.entries.partition_point(|(k, _)| k.as_slice() < start);
        }
        let done = idx >= leaf.entries.len() && leaf.next == NIL_PAGE;
        Ok(BTreeScan {
            pager,
            leaf_id: page_id,
            idx,
            done,
            end: end.map(|e| e.to_vec()),
        })
    }

    /// Multi-line human-readable dump of the whole tree — the text twin of
    /// the future browser visualizer, and the `zdb demo-tree` output.
    pub fn debug_dump(&self, pager: &mut dyn PageIo) -> Result<Vec<String>, DbError> {
        let mut lines = Vec::new();
        if let Some(root) = self.root {
            dump(pager, root, 0, &mut lines)?;
        } else {
            lines.push("(empty tree)".to_string());
        }
        Ok(lines)
    }

    /// Walk the whole tree validating every structural invariant: page
    /// types, ordering, uniqueness, occupancy, and that the leaf chain
    /// replays exactly the in-order key sequence.
    pub fn check_integrity(&self, pager: &mut dyn PageIo) -> Result<(), DbError> {
        let Some(root) = self.root else {
            return Ok(());
        };
        let mut keys = Vec::new();
        let mut chain = Vec::new();
        walk(pager, root, true, &mut keys, &mut chain)?;

        let mut chain_keys = Vec::new();
        let mut page_id = *chain.first().ok_or(DbError::Corrupt("empty leaf chain"))?;
        loop {
            let leaf = read_leaf(pager, page_id)?;
            for (k, _) in &leaf.entries {
                chain_keys.push(k.clone());
            }
            match leaf.next {
                NIL_PAGE => break,
                next => page_id = next,
            }
        }
        if keys != chain_keys {
            return Err(DbError::Corrupt("leaf chain disagrees with tree order"));
        }
        if !keys.windows(2).all(|w| w[0] < w[1]) {
            return Err(DbError::Corrupt("keys not strictly ascending"));
        }
        Ok(())
    }
}

fn walk(
    pager: &mut dyn PageIo,
    page_id: PageId,
    is_root: bool,
    keys: &mut Vec<Vec<u8>>,
    chain: &mut Vec<PageId>,
) -> Result<(), DbError> {
    let page = pager.read_page(page_id)?;
    match page.page_type()? {
        PageType::BTreeInterior => {
            let node = Interior::decode(page.payload())?;
            if !is_root && node.entries.len() < MIN_ENTRIES {
                return Err(DbError::Corrupt("interior node underflow"));
            }
            walk(pager, node.first_child, false, keys, chain)?;
            let mut prev_sep: Option<&Vec<u8>> = None;
            for (sep, child) in &node.entries {
                if let Some(p) = prev_sep
                    && p >= sep
                {
                    return Err(DbError::Corrupt("interior separators not ascending"));
                }
                prev_sep = Some(sep);
                walk(pager, *child, false, keys, chain)?;
            }
        }
        PageType::BTreeLeaf => {
            let leaf = Leaf::decode(page.payload())?;
            if !is_root && leaf.entries.len() < MIN_ENTRIES {
                return Err(DbError::Corrupt("leaf underflow"));
            }
            chain.push(page_id);
            keys.extend(leaf.entries.iter().map(|(k, _)| k.clone()));
        }
        _ => return Err(DbError::Corrupt("unexpected page type in tree")),
    }
    Ok(())
}

fn dump(
    pager: &mut dyn PageIo,
    page_id: PageId,
    depth: usize,
    lines: &mut Vec<String>,
) -> Result<(), DbError> {
    let page = pager.read_page(page_id)?;
    let indent = "  ".repeat(depth);
    match page.page_type()? {
        PageType::BTreeInterior => {
            let node = Interior::decode(page.payload())?;
            lines.push(format!(
                "{indent}interior page {page_id}: {} seps",
                node.entries.len()
            ));
            lines.push(format!("{indent}  -> first child {}", node.first_child));
            for (sep, child) in &node.entries {
                let sep_text = String::from_utf8_lossy(sep);
                lines.push(format!("{indent}  --[{sep_text}]--> child {child}"));
            }
            dump(pager, node.first_child, depth + 1, lines)?;
            for (_, child) in &node.entries {
                dump(pager, *child, depth + 1, lines)?;
            }
        }
        PageType::BTreeLeaf => {
            let leaf = Leaf::decode(page.payload())?;
            let entries: Vec<String> = leaf
                .entries
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}={}",
                        String::from_utf8_lossy(k),
                        String::from_utf8_lossy(v)
                    )
                })
                .collect();
            lines.push(format!(
                "{indent}leaf page {page_id}: [{}] (prev {}, next {})",
                entries.join(", "),
                leaf.prev,
                leaf.next
            ));
        }
        _ => return Err(DbError::Corrupt("unexpected page type in dump")),
    }
    Ok(())
}

fn insert_rec(
    pager: &mut dyn PageIo,
    page_id: PageId,
    key: &[u8],
    value: &[u8],
) -> Result<Option<Split>, DbError> {
    let page = pager.read_page(page_id)?;
    match page.page_type()? {
        PageType::BTreeLeaf => {
            let mut leaf = Leaf::decode(page.payload())?;
            match leaf
                .entries
                .binary_search_by(|(k, _)| k.as_slice().cmp(key))
            {
                Ok(i) => leaf.entries[i].1 = value.to_vec(), // overwrite in place
                Err(i) => leaf.entries.insert(i, (key.to_vec(), value.to_vec())),
            }
            if LEAF_HEADER + leaf_bytes(&leaf.entries) <= PAYLOAD_LEN {
                write_leaf(pager, page_id, &leaf)?;
                return Ok(None);
            }
            // Split: byte-weighted middle so fat entries don't skew halves.
            let total = leaf_bytes(&leaf.entries);
            let mut acc = 0;
            let mut split_at = leaf.entries.len() / 2;
            for (i, (k, v)) in leaf.entries.iter().enumerate() {
                acc += leaf_entry_size(k.len(), v.len());
                if acc * 2 >= total {
                    split_at = i.max(1);
                    break;
                }
            }
            let first_right = leaf.entries[split_at].0.clone();
            let right_entries: Vec<(Vec<u8>, Vec<u8>)> = leaf.entries.split_off(split_at);
            let right_id = pager.alloc_page(PageType::BTreeLeaf)?.page_id();
            let right = Leaf {
                prev: page_id,
                next: leaf.next,
                entries: right_entries,
            };
            leaf.next = right_id;
            write_leaf(pager, page_id, &leaf)?;
            write_leaf(pager, right_id, &right)?;
            Ok(Some(Split {
                sep: first_right,
                right: right_id,
            }))
        }
        PageType::BTreeInterior => {
            let mut node = Interior::decode(page.payload())?;
            let i = child_index(&node.entries, key);
            let child = if i == 0 {
                node.first_child
            } else {
                node.entries[i - 1].1
            };
            if let Some(split) = insert_rec(pager, child, key, value)? {
                let pos = child_index(&node.entries, &split.sep).max(i);
                node.entries.insert(pos, (split.sep, split.right));
                if INTERIOR_HEADER + interior_bytes(&node.entries) <= PAYLOAD_LEN {
                    write_interior(pager, page_id, &node)?;
                    return Ok(None);
                }
                // Interior split: the middle separator is promoted to the
                // parent and removed from both halves.
                let mid = node.entries.len() / 2;
                let right_first_child = node.entries[mid].1;
                let right_entries = node.entries.split_off(mid + 1);
                let promoted = node.entries.pop().unwrap().0;
                let right = Interior {
                    first_child: right_first_child,
                    entries: right_entries,
                };
                write_interior(pager, page_id, &node)?;
                let right_id = pager.alloc_page(PageType::BTreeInterior)?.page_id();
                write_interior(pager, right_id, &right)?;
                return Ok(Some(Split {
                    sep: promoted,
                    right: right_id,
                }));
            }
            Ok(None)
        }
        _ => Err(DbError::Corrupt("unexpected page type in insert")),
    }
}

/// Returns `(deleted, child_underflowed)`.
fn delete_rec(
    pager: &mut dyn PageIo,
    page_id: PageId,
    key: &[u8],
) -> Result<(bool, bool), DbError> {
    let page = pager.read_page(page_id)?;
    match page.page_type()? {
        PageType::BTreeLeaf => {
            let mut leaf = Leaf::decode(page.payload())?;
            match leaf
                .entries
                .binary_search_by(|(k, _)| k.as_slice().cmp(key))
            {
                Ok(i) => {
                    leaf.entries.remove(i);
                    write_leaf(pager, page_id, &leaf)?;
                    Ok((true, leaf.entries.len() < MIN_ENTRIES))
                }
                Err(_) => Ok((false, false)),
            }
        }
        PageType::BTreeInterior => {
            let mut node = Interior::decode(page.payload())?;
            let child_idx = child_index(&node.entries, key);
            let child_id = if child_idx == 0 {
                node.first_child
            } else {
                node.entries[child_idx - 1].1
            };
            let (deleted, underflow) = delete_rec(pager, child_id, key)?;
            if !underflow {
                return Ok((deleted, false));
            }

            let left_id = child_idx.checked_sub(1).map(|i| node.entries[i].1);
            let right_id = node.entries.get(child_idx).map(|(_, id)| *id);

            if let Some(left_id) = left_id
                && sibling_has_spare(pager, left_id)?
            {
                borrow_from_left(pager, &mut node, child_idx, left_id)?;
                write_interior(pager, page_id, &node)?;
                return Ok((deleted, false));
            }
            if let Some(right_id) = right_id
                && sibling_has_spare(pager, right_id)?
            {
                borrow_from_right(pager, &mut node, child_idx, right_id)?;
                write_interior(pager, page_id, &node)?;
                return Ok((deleted, false));
            }

            // Merge with a sibling (prefer left) and drop one separator.
            // `None` for both siblings means we are the root's only child;
            // `BTree::delete` collapses the root instead.
            let (merged_into, merged_page, sep_pos) = match (left_id, right_id) {
                (Some(left_id), _) => (left_id, child_id, child_idx - 1),
                (None, Some(right_id)) => (child_id, right_id, child_idx),
                (None, None) => return Ok((deleted, true)),
            };
            merge(pager, &mut node, sep_pos, merged_into, merged_page)?;
            pager.free_page(merged_page)?;
            write_interior(pager, page_id, &node)?;
            Ok((deleted, node.entries.len() < MIN_ENTRIES))
        }
        _ => Err(DbError::Corrupt("unexpected page type in delete")),
    }
}

fn sibling_has_spare(pager: &mut dyn PageIo, page_id: PageId) -> Result<bool, DbError> {
    let page = pager.read_page(page_id)?;
    let count = match page.page_type()? {
        PageType::BTreeLeaf => Leaf::decode(page.payload())?.entries.len(),
        PageType::BTreeInterior => Interior::decode(page.payload())?.entries.len(),
        _ => return Err(DbError::Corrupt("unexpected sibling page type")),
    };
    Ok(count > MIN_ENTRIES)
}

fn borrow_from_left(
    pager: &mut dyn PageIo,
    parent: &mut Interior,
    child_idx: usize,
    left_id: PageId,
) -> Result<(), DbError> {
    let sep_idx = child_idx - 1;
    let parent_sep = parent.entries[sep_idx].0.clone();
    let child_id = parent.entries[child_idx].1;
    if pager.read_page(child_id)?.page_type()? == PageType::BTreeLeaf {
        let mut left = read_leaf(pager, left_id)?;
        let mut child = read_leaf(pager, child_id)?;
        let moved = left.entries.pop().unwrap();
        child.entries.insert(0, moved);
        parent.entries[sep_idx].0 = child.entries[0].0.clone();
        write_leaf(pager, left_id, &left)?;
        write_leaf(pager, child_id, &child)?;
    } else {
        let mut left = read_interior(pager, left_id)?;
        let mut child = read_interior(pager, child_id)?;
        let (last_sep, last_child) = left.entries.pop().unwrap();
        child.entries.insert(0, (parent_sep, child.first_child));
        child.first_child = last_child;
        parent.entries[sep_idx].0 = last_sep;
        write_interior(pager, left_id, &left)?;
        write_interior(pager, child_id, &child)?;
    }
    Ok(())
}

fn borrow_from_right(
    pager: &mut dyn PageIo,
    parent: &mut Interior,
    child_idx: usize,
    right_id: PageId,
) -> Result<(), DbError> {
    let sep_idx = child_idx;
    let parent_sep = parent.entries[sep_idx].0.clone();
    let child_id = parent.entries[child_idx].1;
    if pager.read_page(child_id)?.page_type()? == PageType::BTreeLeaf {
        let mut right = read_leaf(pager, right_id)?;
        let mut child = read_leaf(pager, child_id)?;
        let moved = right.entries.remove(0);
        child.entries.push(moved);
        parent.entries[sep_idx].0 = right.entries[0].0.clone();
        write_leaf(pager, child_id, &child)?;
        write_leaf(pager, right_id, &right)?;
    } else {
        let mut right = read_interior(pager, right_id)?;
        let mut child = read_interior(pager, child_id)?;
        let (first_sep, first_child) = right.entries.remove(0);
        child.entries.push((parent_sep, right.first_child));
        right.first_child = first_child;
        parent.entries[sep_idx].0 = first_sep;
        write_interior(pager, child_id, &child)?;
        write_interior(pager, right_id, &right)?;
    }
    Ok(())
}

fn merge(
    pager: &mut dyn PageIo,
    parent: &mut Interior,
    sep_pos: usize,
    into_id: PageId,
    from_id: PageId,
) -> Result<(), DbError> {
    let sep = parent.entries[sep_pos].0.clone();
    parent.entries.remove(sep_pos);
    if pager.read_page(into_id)?.page_type()? == PageType::BTreeLeaf {
        let mut into = read_leaf(pager, into_id)?;
        let from = read_leaf(pager, from_id)?;
        into.entries.extend(from.entries);
        into.next = from.next;
        write_leaf(pager, into_id, &into)?;
        if from.next != NIL_PAGE {
            let mut nxt = read_leaf(pager, from.next)?;
            nxt.prev = into_id;
            write_leaf(pager, from.next, &nxt)?;
        }
    } else {
        let mut into = read_interior(pager, into_id)?;
        let from = read_interior(pager, from_id)?;
        into.entries.push((sep, from.first_child));
        into.entries.extend(from.entries);
        write_interior(pager, into_id, &into)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Range scan iterator
// ---------------------------------------------------------------------------

/// Ascending iterator over every `(key, value)` in the tree, following the
/// leaf chain page by page. An optional exclusive end bound stops it early.
pub struct BTreeScan<'a> {
    pager: &'a mut dyn PageIo,
    leaf_id: PageId,
    idx: usize,
    done: bool,
    end: Option<Vec<u8>>,
}

impl<'a> Iterator for BTreeScan<'a> {
    type Item = (Vec<u8>, Vec<u8>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.done {
                return None;
            }
            // A corrupt page mid-scan ends iteration; the integrity checker
            // is the tool that reports corruption, not the iterator.
            let leaf = read_leaf(self.pager, self.leaf_id).ok()?;
            if self.idx < leaf.entries.len() {
                let entry = leaf.entries[self.idx].clone();
                if let Some(end) = &self.end
                    && entry.0.as_slice() >= end.as_slice()
                {
                    self.done = true;
                    return None;
                }
                self.idx += 1;
                return Some(entry);
            }
            if leaf.next == NIL_PAGE {
                self.done = true;
                return None;
            }
            self.leaf_id = leaf.next;
            self.idx = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pager::Pager;
    use crate::testing::TempDir;

    fn opened(tag: &str) -> (TempDir, Pager) {
        let dir = TempDir::new(tag);
        let pager = Pager::create(dir.path().join("t.zdb")).unwrap();
        (dir, pager)
    }

    #[test]
    fn insert_get_overwrite_delete() {
        let (_dir, mut pager) = opened("basic");
        let mut tree = BTree::empty();
        assert_eq!(tree.get(&mut pager, b"k").unwrap(), None);
        assert!(!tree.delete(&mut pager, b"k").unwrap());

        tree.insert(&mut pager, b"k1", b"v1").unwrap();
        tree.insert(&mut pager, b"k3", b"v3").unwrap();
        tree.insert(&mut pager, b"k2", b"v2").unwrap();
        assert_eq!(
            tree.get(&mut pager, b"k2").unwrap().as_deref(),
            Some(b"v2".as_ref())
        );

        tree.insert(&mut pager, b"k2", b"v2-new").unwrap();
        assert_eq!(
            tree.get(&mut pager, b"k2").unwrap().as_deref(),
            Some(b"v2-new".as_ref())
        );

        assert!(tree.delete(&mut pager, b"k2").unwrap());
        assert_eq!(tree.get(&mut pager, b"k2").unwrap(), None);
        tree.check_integrity(&mut pager).unwrap();
    }

    #[test]
    fn scan_yields_sorted_entries() {
        let (_dir, mut pager) = opened("scan");
        let mut tree = BTree::empty();
        for i in 0..100u32 {
            tree.insert(&mut pager, format!("key-{i:03}").as_bytes(), b"v")
                .unwrap();
        }
        let keys: Vec<Vec<u8>> = tree.scan(&mut pager).unwrap().map(|(k, _)| k).collect();
        let expected: Vec<Vec<u8>> = (0..100u32)
            .map(|i| format!("key-{i:03}").into_bytes())
            .collect();
        assert_eq!(keys, expected);
        tree.check_integrity(&mut pager).unwrap();
    }

    #[test]
    fn empty_tree_scan_is_empty() {
        let (_dir, mut pager) = opened("empty");
        let tree = BTree::empty();
        assert_eq!(tree.scan(&mut pager).unwrap().count(), 0);
        tree.check_integrity(&mut pager).unwrap();
    }

    #[test]
    fn oversized_entries_are_rejected() {
        let (_dir, mut pager) = opened("big");
        let mut tree = BTree::empty();
        let err = tree
            .insert(&mut pager, &[b'k'; MAX_KEY_LEN + 1], b"v")
            .unwrap_err();
        assert!(matches!(err, DbError::EntryTooLarge { .. }));
    }
}
