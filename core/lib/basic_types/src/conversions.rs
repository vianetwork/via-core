//! Conversions between basic types.

use bigdecimal::BigDecimal;
use num::BigUint;

use crate::{Address, H256, U256};

pub fn h256_to_u256(num: H256) -> U256 {
    U256::from_big_endian(num.as_bytes())
}

pub fn address_to_h256(address: &Address) -> H256 {
    let mut buffer = [0u8; 32];
    buffer[12..].copy_from_slice(address.as_bytes());
    H256(buffer)
}

pub fn address_to_u256(address: &Address) -> U256 {
    h256_to_u256(address_to_h256(address))
}

pub fn u256_to_h256(num: U256) -> H256 {
    let mut bytes = [0u8; 32];
    num.to_big_endian(&mut bytes);
    H256::from_slice(&bytes)
}

/// Converts `U256` value into an [`Address`].
pub fn u256_to_address(value: &U256) -> Address {
    let mut bytes = [0u8; 32];
    value.to_big_endian(&mut bytes);

    Address::from_slice(&bytes[12..])
}

/// Converts `H256` value into an [`Address`].
pub fn h256_to_address(value: &H256) -> Address {
    Address::from_slice(&value.as_bytes()[12..])
}

/// Converts `U256` value into bytes array
pub fn u256_to_bytes_be(value: &U256) -> Vec<u8> {
    let mut bytes = vec![0u8; 32];
    value.to_big_endian(bytes.as_mut_slice());
    bytes
}

pub fn u256_to_big_decimal(value: U256) -> BigDecimal {
    let mut u32_digits = vec![0_u32; 8];
    // `u64_digit`s from `U256` are little-endian
    for (i, &u64_digit) in value.0.iter().enumerate() {
        u32_digits[2 * i] = u64_digit as u32;
        u32_digits[2 * i + 1] = (u64_digit >> 32) as u32;
    }
    let value = BigUint::new(u32_digits);
    BigDecimal::new(value.into(), 0)
}

/// Converts `BigUint` value into the corresponding `U256` value.
fn biguint_to_u256(value: BigUint) -> U256 {
    let bytes = value.to_bytes_le();
    U256::from_little_endian(&bytes)
}

/// Converts `BigDecimal` value into the corresponding `U256` value.
pub fn bigdecimal_to_u256(value: BigDecimal) -> U256 {
    let bigint = value.with_scale(0).into_bigint_and_exponent().0;
    biguint_to_u256(bigint.to_biguint().unwrap())
}
