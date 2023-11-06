use crate::{
    asm_generation::from_ir::ir_type_size_in_bytes,
    fuel_prelude::{
        fuel_crypto::Hasher,
        fuel_tx::StorageSlot,
        fuel_types::{Bytes32, Bytes8},
    },
    size_bytes_round_up_to_word_alignment,
};
use sway_ir::{
    constant::{Constant, ConstantValue},
    context::Context,
    irtype::Type,
};
use sway_types::state::StateIndex;

/// Determines how values that are less then a word in length
/// has to be padded to word boundary when in structs or enums.
#[derive(Default)]
enum InByte8Padding {
    #[default]
    Right,
    Left,
}

/// Hands out storage keys using a state index and a list of subfield indices.
/// Basically returns sha256("storage_<state_index>_<idx1>_<idx2>_..")
pub(super) fn get_storage_key<T>(ix: &StateIndex, indices: &[T]) -> Bytes32
where
    T: std::fmt::Display,
{
    Hasher::hash(indices.iter().fold(
        format!(
            "{}{}",
            sway_utils::constants::STORAGE_DOMAIN_SEPARATOR,
            ix.to_usize()
        ),
        |acc, i| format!("{acc}_{i}"),
    ))
}

use uint::construct_uint;

#[allow(
// These two warnings are generated by the `construct_uint!()` macro below.
    clippy::assign_op_pattern,
    clippy::ptr_offset_with_cast
)]
pub(super) fn add_to_b256(x: Bytes32, y: u64) -> Bytes32 {
    construct_uint! {
        struct U256(4);
    }
    let x = U256::from(*x);
    let y = U256::from(y);
    let res: [u8; 32] = (x + y).into();
    Bytes32::from(res)
}

/// Given a constant value `constant`, a type `ty`, a state index, and a vector of subfield
/// indices, serialize the constant into a vector of storage slots. The keys (slots) are
/// generated using the state index and the subfield indices which are recursively built. The
/// values are generated such that each subfield gets its own storage slot except for enums and
/// strings which are spread over successive storage slots (use `serialize_to_words` in this case).
///
/// This behavior matches the behavior of how storage slots are assigned for storage reads and
/// writes (i.e. how `state_read_*` and `state_write_*` instructions are generated).
pub fn serialize_to_storage_slots(
    constant: &Constant,
    context: &Context,
    ix: &StateIndex,
    ty: &Type,
    indices: &[usize],
) -> Vec<StorageSlot> {
    match &constant.value {
        ConstantValue::Undef => vec![],
        // If not being a part of an aggregate, single byte values like `bool`, `u8`, and unit
        // are stored as a byte at the beginning of the storage slot.
        ConstantValue::Unit if ty.is_unit(context) => vec![StorageSlot::new(
            get_storage_key(ix, indices),
            Bytes32::new([0; 32]),
        )],
        ConstantValue::Bool(b) if ty.is_bool(context) => {
            vec![StorageSlot::new(
                get_storage_key(ix, indices),
                Bytes32::new([
                    if *b { 1 } else { 0 },
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                ]),
            )]
        }
        ConstantValue::Uint(b) if ty.is_uint8(context) => {
            vec![StorageSlot::new(
                get_storage_key(ix, indices),
                Bytes32::new([
                    *b as u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0,
                ]),
            )]
        }
        // Similarly, other uint values are stored at the beginning of the storage slot.
        ConstantValue::Uint(n) if ty.is_uint(context) => {
            vec![StorageSlot::new(
                get_storage_key(ix, indices),
                Bytes32::new(
                    n.to_be_bytes()
                        .iter()
                        .cloned()
                        .chain([0; 24].iter().cloned())
                        .collect::<Vec<u8>>()
                        .try_into()
                        .unwrap(),
                ),
            )]
        }
        ConstantValue::U256(b) if ty.is_uint_of(context, 256) => {
            vec![StorageSlot::new(
                get_storage_key(ix, indices),
                Bytes32::new(b.to_be_bytes()),
            )]
        }
        ConstantValue::B256(b) if ty.is_b256(context) => {
            vec![StorageSlot::new(
                get_storage_key(ix, indices),
                Bytes32::new(b.to_be_bytes()),
            )]
        }
        ConstantValue::Array(_a) if ty.is_array(context) => {
            unimplemented!("Arrays in storage have not been implemented yet.")
        }
        _ if ty.is_string_array(context) || ty.is_struct(context) || ty.is_union(context) => {
            // Serialize the constant data in words and add zero words until the number of words
            // is a multiple of 4. This is useful because each storage slot is 4 words.
            // Regarding padding, the top level type in the call is either a string array, struct, or
            // a union. They will properly set the initial padding for the further recursive calls.
            let mut packed = serialize_to_words(constant, context, ty, InByte8Padding::default());
            packed.extend(vec![
                Bytes8::new([0; 8]);
                ((packed.len() + 3) / 4) * 4 - packed.len()
            ]);

            assert!(packed.len() % 4 == 0);

            // Return a list of `StorageSlot`s
            // First get the keys then get the values
            (0..(ir_type_size_in_bytes(context, ty) + 31) / 32)
                .map(|i| add_to_b256(get_storage_key(ix, indices), i))
                .zip((0..packed.len() / 4).map(|i| {
                    Bytes32::new(
                        Vec::from_iter((0..4).flat_map(|j| *packed[4 * i + j]))
                            .try_into()
                            .unwrap(),
                    )
                }))
                .map(|(k, r)| StorageSlot::new(k, r))
                .collect()
        }
        _ => vec![],
    }
}

/// Given a constant value `constant` and a type `ty`, serialize the constant into a vector of
/// words and apply the requested padding if needed.
fn serialize_to_words(constant: &Constant, context: &Context, ty: &Type, padding: InByte8Padding) -> Vec<Bytes8> {
    match &constant.value {
        ConstantValue::Undef => vec![],
        ConstantValue::Unit if ty.is_unit(context) => vec![Bytes8::new([0; 8])],
        ConstantValue::Bool(b) if ty.is_bool(context) => {
            match padding {
                InByte8Padding::Right => vec![Bytes8::new([if *b { 1 } else { 0 }, 0, 0, 0, 0, 0, 0, 0])],
                InByte8Padding::Left => vec![Bytes8::new([0, 0, 0, 0, 0, 0, 0, if *b { 1 } else { 0 }])],
            }
        }
        ConstantValue::Uint(n) if ty.is_uint8(context) => {
            match padding {
                InByte8Padding::Right => vec![Bytes8::new([*n as u8, 0, 0, 0, 0, 0, 0, 0])],
                InByte8Padding::Left => vec![Bytes8::new([0, 0, 0, 0, 0, 0, 0, *n as u8])],
            }
        }
        ConstantValue::Uint(n) if ty.is_uint(context) => {
            vec![Bytes8::new(n.to_be_bytes())]
        }
        ConstantValue::U256(b) if ty.is_uint_of(context, 256) => {
            let b = b.to_be_bytes();
            Vec::from_iter((0..4).map(|i| Bytes8::new(b[8 * i..8 * i + 8].try_into().unwrap())))
        }
        ConstantValue::B256(b) if ty.is_b256(context) => {
            let b = b.to_be_bytes();
            Vec::from_iter((0..4).map(|i| Bytes8::new(b[8 * i..8 * i + 8].try_into().unwrap())))
        }
        ConstantValue::String(s) if ty.is_string_array(context) => {
            // Turn the bytes into serialized words (Bytes8) and right pad it to the word boundary.
            let mut s = s.clone();
            s.extend(vec![0; ((s.len() + 7) / 8) * 8 - s.len()]);

            assert!(s.len() % 8 == 0);

            // Group into words.
            Vec::from_iter((0..s.len() / 8).map(|i| {
                Bytes8::new(
                    Vec::from_iter((0..8).map(|j| s[8 * i + j]))
                        .try_into()
                        .unwrap(),
                )
            }))
        }
        ConstantValue::Array(_) if ty.is_array(context) => {
            unimplemented!("Arrays in storage have not been implemented yet.")
        }
        ConstantValue::Struct(vec) if ty.is_struct(context) => {
            let field_tys = ty.get_field_types(context);
            vec.iter()
                .zip(field_tys.iter())
                .flat_map(|(f, ty)| serialize_to_words(f, context, ty, InByte8Padding::Right))
                .collect()
        }
        _ if ty.is_union(context) => {
            let value_size_in_words =
                size_bytes_round_up_to_word_alignment!(ir_type_size_in_bytes(context, ty)) / 8;
            let constant_size_in_words = size_bytes_round_up_to_word_alignment!(
                ir_type_size_in_bytes(context, &constant.ty)
            ) / 8;

            assert!(value_size_in_words >= constant_size_in_words);

            // Add enough left padding to satisfy the actual size of the union
            let padding_size_in_words = value_size_in_words - constant_size_in_words;
            vec![Bytes8::new([0; 8]); padding_size_in_words as usize]
                .iter()
                .cloned()
                .chain(
                    serialize_to_words(constant, context, &constant.ty, InByte8Padding::Left)
                        .iter()
                        .cloned(),
                )
                .collect()
        }
        _ => vec![],
    }
}
