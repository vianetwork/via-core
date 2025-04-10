use zksync_basic_types::H256;

pub fn reverse_vec_to_h256(hash: Vec<u8>) -> H256 {
    let mut hash = hash;
    hash.reverse();
    H256::from_slice(&hash)
}
