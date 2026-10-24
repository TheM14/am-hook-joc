//! JOC Huffman decoding.

use crate::bits::BitReader;
use crate::tables_huffman as tables;
use crate::ErrorKind;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Mtx,
    Idx,
    Vec,
}

fn select_tree(quant_idx: u8, kind: SymbolKind, n_channels: u8) -> &'static [[i16; 2]] {
    match kind {
        SymbolKind::Idx => {
            if n_channels == 5 {
                &tables::POS_INDEX_5CH
            } else {
                &tables::POS_INDEX_7CH
            }
        }
        SymbolKind::Vec => {
            if quant_idx == 0 {
                &tables::COARSE_COEFF_SPARSE
            } else {
                &tables::FINE_COEFF_SPARSE
            }
        }
        SymbolKind::Mtx => {
            if quant_idx == 0 {
                &tables::COARSE_GENERIC
            } else {
                &tables::FINE_GENERIC
            }
        }
    }
}

/// Walks the tree until a leaf; leaves are stored as `-symbol - 1`.
pub fn decode(quant_idx: u8, kind: SymbolKind, n_channels: u8, reader: &mut BitReader<'_>) -> Option<i16> {
    let tree = select_tree(quant_idx, kind, n_channels);
    let count = tree.len();
    let mut node: i32 = 0;
    let mut steps = 0usize;
    while node >= 0 {
        if node as usize >= count || steps > count {
            reader.fail(ErrorKind::JocSyntax, "Huffman tree walk left the valid node range");
            return None;
        }
        steps += 1;
        let bit = reader.read(1) as usize;
        if reader.failed() {
            return None;
        }
        node = i32::from(tree[node as usize][bit]);
    }
    Some((-node - 1) as i16)
}
