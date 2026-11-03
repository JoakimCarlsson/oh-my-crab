//! The 64-bit xxHash used by Claude Code's request attestation.

/// First xxHash64 prime.
const P1: u64 = 0x9E37_79B1_85EB_CA87;

/// Second xxHash64 prime.
const P2: u64 = 0xC2B2_AE3D_27D4_EB4F;

/// Third xxHash64 prime.
const P3: u64 = 0x1656_67B1_9E37_79F9;

/// Fourth xxHash64 prime.
const P4: u64 = 0x85EB_CA77_C2B2_AE63;

/// Fifth xxHash64 prime.
const P5: u64 = 0x27D4_EB2F_1656_67C5;

/// Reads a little-endian `u64` from the first eight bytes of `bytes`.
fn read64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes[..8].try_into().expect("eight bytes"))
}

/// Reads a little-endian `u32` from the first four bytes of `bytes`.
fn read32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().expect("four bytes"))
}

/// Folds one 64-bit lane `input` into the accumulator `acc`.
fn round(acc: u64, input: u64) -> u64 {
    acc.wrapping_add(input.wrapping_mul(P2))
        .rotate_left(31)
        .wrapping_mul(P1)
}

/// Merges accumulator lane `value` into the running hash `acc`.
fn merge(acc: u64, value: u64) -> u64 {
    (acc ^ round(0, value)).wrapping_mul(P1).wrapping_add(P4)
}

/// Computes the xxHash64 of `data` with `seed`.
pub fn xxh64(data: &[u8], seed: u64) -> u64 {
    let mut rest = data;
    let mut hash;
    if data.len() >= 32 {
        let mut lanes = [
            seed.wrapping_add(P1).wrapping_add(P2),
            seed.wrapping_add(P2),
            seed,
            seed.wrapping_sub(P1),
        ];
        while rest.len() >= 32 {
            for (index, lane) in lanes.iter_mut().enumerate() {
                *lane = round(*lane, read64(&rest[index * 8..]));
            }
            rest = &rest[32..];
        }
        hash = lanes[0]
            .rotate_left(1)
            .wrapping_add(lanes[1].rotate_left(7))
            .wrapping_add(lanes[2].rotate_left(12))
            .wrapping_add(lanes[3].rotate_left(18));
        for lane in lanes {
            hash = merge(hash, lane);
        }
    } else {
        hash = seed.wrapping_add(P5);
    }
    hash = hash.wrapping_add(data.len() as u64);
    while rest.len() >= 8 {
        hash = (hash ^ round(0, read64(rest)))
            .rotate_left(27)
            .wrapping_mul(P1)
            .wrapping_add(P4);
        rest = &rest[8..];
    }
    if rest.len() >= 4 {
        hash = (hash ^ u64::from(read32(rest)).wrapping_mul(P1))
            .rotate_left(23)
            .wrapping_mul(P2)
            .wrapping_add(P3);
        rest = &rest[4..];
    }
    for byte in rest {
        hash = (hash ^ u64::from(*byte).wrapping_mul(P5))
            .rotate_left(11)
            .wrapping_mul(P1);
    }
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(P2);
    hash ^= hash >> 29;
    hash = hash.wrapping_mul(P3);
    hash ^ (hash >> 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Matches the reference implementation's published test vectors.
    #[test]
    fn matches_reference_vectors() {
        assert_eq!(xxh64(b"", 0), 0xEF46_DB37_51D8_E999);
        assert_eq!(xxh64(b"abc", 0), 0x44BC_2CF5_AD77_0999);
    }

    /// Matches Bun's `xxHash64` on multi-stripe inputs with and without a seed.
    #[test]
    fn matches_bun_on_long_inputs() {
        let data: Vec<u8> = (0..100u8).collect();
        assert_eq!(xxh64(&data, 0x4d65_9218_e32a_3268), 0x4ed2_b498_82e0_9e04);
        assert_eq!(xxh64(&data[..40], 0), 0xf5da_40f1_b117_41e9);
    }

    /// Inputs of every tail length and over one stripe hash without panicking and differ.
    #[test]
    fn covers_every_length_class() {
        let data: Vec<u8> = (0..100u8).collect();
        let mut seen = std::collections::HashSet::new();
        for len in 0..data.len() {
            assert!(seen.insert(xxh64(&data[..len], 0x4d65_9218_e32a_3268)));
        }
    }
}
