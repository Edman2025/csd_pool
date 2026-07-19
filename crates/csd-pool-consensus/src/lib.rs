use sha2::{Digest, Sha256};
use thiserror::Error;

pub type Hash32 = [u8; 32];

#[derive(Debug, Error)]
pub enum ConsensusError {
    #[error("{field} must be {expected} bytes, got {actual}")]
    WrongLength {
        field: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("{0} is invalid hex")]
    InvalidHex(&'static str),
    #[error("{0} is invalid integer hex")]
    InvalidIntegerHex(&'static str),
    #[error("share does not meet assigned target")]
    LowDifficultyShare,
}

pub type Result<T> = std::result::Result<T, ConsensusError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkTemplate {
    pub job_id: String,
    pub version: u32,
    pub prev: Hash32,
    pub time: u64,
    pub bits: u32,
    pub share_target: Hash32,
    pub network_target: Hash32,
    pub coinbase_prefix: Vec<u8>,
    pub coinbase_suffix: Vec<u8>,
    pub merkle_branch: Vec<Hash32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitSolution {
    pub extranonce2_le: [u8; 4],
    pub ntime: u32,
    pub nonce: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedShare {
    pub hash: Hash32,
    pub header: [u8; 84],
    pub coinbase_txid: Hash32,
    pub merkle_root: Hash32,
    pub is_block_candidate: bool,
}

pub fn decode_hash32_hex(field: &'static str, value: &str) -> Result<Hash32> {
    let bytes = hex::decode(value).map_err(|_| ConsensusError::InvalidHex(field))?;
    hash32_from_slice(field, &bytes)
}

pub fn decode_prev_hash_from_stratum(value: &str) -> Result<Hash32> {
    let mut hash = decode_hash32_hex("prev_hash_be_hex", value)?;
    hash.reverse();
    Ok(hash)
}

pub fn parse_u32_hex(field: &'static str, value: &str) -> Result<u32> {
    u32::from_str_radix(value, 16).map_err(|_| ConsensusError::InvalidIntegerHex(field))
}

pub fn parse_le_u32_hex_bytes(field: &'static str, value: &str) -> Result<[u8; 4]> {
    let bytes = hex::decode(value).map_err(|_| ConsensusError::InvalidHex(field))?;
    if bytes.len() != 4 {
        return Err(ConsensusError::WrongLength {
            field,
            expected: 4,
            actual: bytes.len(),
        });
    }
    let mut out = [0u8; 4];
    out.copy_from_slice(&bytes);
    Ok(out)
}

pub fn coinbase_bytes(prefix: &[u8], extranonce: u64, suffix: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(prefix.len() + 8 + suffix.len());
    out.extend_from_slice(prefix);
    out.extend_from_slice(&extranonce.to_le_bytes());
    out.extend_from_slice(suffix);
    out
}

pub fn coinbase_txid(prefix: &[u8], extranonce: u64, suffix: &[u8]) -> Hash32 {
    sha256d(&coinbase_bytes(prefix, extranonce, suffix))
}

pub fn compose_extranonce(extranonce1_le: [u8; 4], extranonce2_le: [u8; 4]) -> u64 {
    let low = u32::from_le_bytes(extranonce1_le) as u64;
    let high = u32::from_le_bytes(extranonce2_le) as u64;
    low | (high << 32)
}

pub fn merkle_root_from_branch(leaf: Hash32, branch: &[Hash32]) -> Hash32 {
    let mut current = leaf;
    for sibling in branch {
        let mut pair = [0u8; 64];
        pair[..32].copy_from_slice(&current);
        pair[32..].copy_from_slice(sibling);
        current = sha256d(&pair);
    }
    current
}

pub fn header_84(
    version: u32,
    prev: &Hash32,
    merkle: &Hash32,
    time: u64,
    bits: u32,
    nonce: u32,
) -> [u8; 84] {
    let mut header = [0u8; 84];
    header[0..4].copy_from_slice(&version.to_le_bytes());
    header[4..36].copy_from_slice(prev);
    header[36..68].copy_from_slice(merkle);
    header[68..76].copy_from_slice(&time.to_le_bytes());
    header[76..80].copy_from_slice(&bits.to_le_bytes());
    header[80..84].copy_from_slice(&nonce.to_le_bytes());
    header
}

pub fn header_hash(header: &[u8; 84]) -> Hash32 {
    sha256d(header)
}

pub fn verify_share(
    template: &WorkTemplate,
    extranonce1_le: [u8; 4],
    solution: &SubmitSolution,
) -> Result<VerifiedShare> {
    verify_share_against_target(template, extranonce1_le, solution, template.share_target)
}

pub fn verify_share_with_difficulty(
    template: &WorkTemplate,
    extranonce1_le: [u8; 4],
    solution: &SubmitSolution,
    difficulty: f64,
) -> Result<VerifiedShare> {
    let assigned_target = target_for_difficulty(&template.share_target, difficulty);
    verify_share_against_target(template, extranonce1_le, solution, assigned_target)
}

fn verify_share_against_target(
    template: &WorkTemplate,
    extranonce1_le: [u8; 4],
    solution: &SubmitSolution,
    assigned_target: Hash32,
) -> Result<VerifiedShare> {
    let extranonce = compose_extranonce(extranonce1_le, solution.extranonce2_le);
    let coinbase_txid = coinbase_txid(
        &template.coinbase_prefix,
        extranonce,
        &template.coinbase_suffix,
    );
    let merkle_root = merkle_root_from_branch(coinbase_txid, &template.merkle_branch);
    let header = header_84(
        template.version,
        &template.prev,
        &merkle_root,
        solution.ntime as u64,
        template.bits,
        solution.nonce,
    );
    let hash = header_hash(&header);
    if !hash_leq_target(&hash, &assigned_target) {
        return Err(ConsensusError::LowDifficultyShare);
    }
    Ok(VerifiedShare {
        hash,
        header,
        coinbase_txid,
        merkle_root,
        is_block_candidate: hash_leq_target(&hash, &template.network_target),
    })
}

pub fn target_for_difficulty(base_target: &Hash32, difficulty: f64) -> Hash32 {
    if !difficulty.is_finite() || difficulty <= 1.0 {
        return *base_target;
    }
    let divisor = if difficulty >= u64::MAX as f64 {
        u64::MAX
    } else {
        difficulty.ceil() as u64
    };
    div_target_by_u64(base_target, divisor.max(1))
}

pub fn difficulty_for_target(base_target: &Hash32, target: &Hash32) -> f64 {
    let Some((base_mantissa, base_shift)) = target_mantissa_and_shift(base_target) else {
        return 0.0;
    };
    let Some((target_mantissa, target_shift)) = target_mantissa_and_shift(target) else {
        return 0.0;
    };
    let shift = base_shift as i32 - target_shift as i32;
    let ratio = base_mantissa / target_mantissa;
    if shift == 0 {
        ratio
    } else {
        ratio * 256_f64.powi(shift)
    }
}

fn target_mantissa_and_shift(target: &Hash32) -> Option<(f64, usize)> {
    let first = target.iter().position(|byte| *byte != 0)?;
    let mut mantissa = 0_u64;
    let mut bytes = 0_usize;
    for byte in target.iter().skip(first).take(8) {
        mantissa = (mantissa << 8) | (*byte as u64);
        bytes += 1;
    }
    if mantissa == 0 {
        return None;
    }
    let remaining_bytes = 32_usize.saturating_sub(first + bytes);
    Some((mantissa as f64, remaining_bytes))
}

fn div_target_by_u64(target: &Hash32, divisor: u64) -> Hash32 {
    if divisor <= 1 {
        return *target;
    }
    let mut out = [0u8; 32];
    let mut rem = 0u128;
    let divisor = divisor as u128;
    for (index, byte) in target.iter().enumerate() {
        let acc = (rem << 8) | (*byte as u128);
        out[index] = (acc / divisor) as u8;
        rem = acc % divisor;
    }
    out
}

pub fn hash_leq_target(hash: &Hash32, target: &Hash32) -> bool {
    hash <= target
}

pub fn sha256d(bytes: &[u8]) -> Hash32 {
    let first = Sha256::digest(bytes);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

fn hash32_from_slice(field: &'static str, bytes: &[u8]) -> Result<Hash32> {
    if bytes.len() != 32 {
        return Err(ConsensusError::WrongLength {
            field,
            expected: 32,
            actual: bytes.len(),
        });
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composes_extranonce_as_xn1_low_xn2_high() {
        let xn1 = [0x01, 0x02, 0x03, 0x04];
        let xn2 = [0x05, 0x06, 0x07, 0x08];
        let extranonce = compose_extranonce(xn1, xn2);
        assert_eq!(extranonce.to_le_bytes(), [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn reverses_prev_hash_from_stratum() {
        let hex: String = (0u8..32).map(|b| format!("{b:02x}")).collect();
        let prev = decode_prev_hash_from_stratum(&hex).unwrap();
        assert_eq!(prev[0], 31);
        assert_eq!(prev[31], 0);
    }

    #[test]
    fn builds_84_byte_header_layout() {
        let prev = [0x11; 32];
        let merkle = [0x22; 32];
        let header = header_84(
            0x20000000,
            &prev,
            &merkle,
            0x0102030405060708,
            0x1d00ffff,
            9,
        );

        assert_eq!(&header[0..4], &0x20000000u32.to_le_bytes());
        assert_eq!(&header[4..36], &prev);
        assert_eq!(&header[36..68], &merkle);
        assert_eq!(&header[68..76], &0x0102030405060708u64.to_le_bytes());
        assert_eq!(&header[76..80], &0x1d00ffffu32.to_le_bytes());
        assert_eq!(&header[80..84], &9u32.to_le_bytes());
    }

    #[test]
    fn coinbase_contains_eight_extranonce_bytes() {
        let bytes = coinbase_bytes(&[0xaa], 0x0807060504030201, &[0xbb]);
        assert_eq!(bytes, vec![0xaa, 1, 2, 3, 4, 5, 6, 7, 8, 0xbb]);
    }

    #[test]
    fn target_compare_uses_big_endian_hash_order() {
        let low = [0x00; 32];
        let mut high = [0x00; 32];
        high[31] = 1;
        assert!(hash_leq_target(&low, &high));
        assert!(!hash_leq_target(&high, &low));
    }

    #[test]
    fn target_for_difficulty_divides_big_endian_target() {
        let base = [0xff; 32];
        let target = target_for_difficulty(&base, 2.0);
        assert_eq!(target[0], 0x7f);
        assert!(target[1..].iter().all(|byte| *byte == 0xff));

        let mut base = [0u8; 32];
        base[0] = 0x80;
        let target = target_for_difficulty(&base, 2.0);
        assert_eq!(target[0], 0x40);
        assert!(target[1..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn target_for_difficulty_ceilings_fractional_values() {
        let base = [0xff; 32];
        assert_eq!(
            target_for_difficulty(&base, 1.1),
            target_for_difficulty(&base, 2.0)
        );
        assert_eq!(target_for_difficulty(&base, 0.0), base);
    }

    #[test]
    fn difficulty_for_target_estimates_relative_work() {
        let base = [0xff; 32];
        let half = target_for_difficulty(&base, 2.0);
        assert!((difficulty_for_target(&base, &base) - 1.0).abs() < f64::EPSILON);
        assert!((difficulty_for_target(&base, &half) - 2.0).abs() < 0.01);
        assert_eq!(difficulty_for_target(&base, &[0; 32]), 0.0);
    }

    #[test]
    fn verifies_share_against_easy_target() {
        let template = WorkTemplate {
            job_id: "job1".to_owned(),
            version: 0x20000000,
            prev: [0; 32],
            time: 0x665544cc,
            bits: 0x207fffff,
            share_target: [0xff; 32],
            network_target: [0; 32],
            coinbase_prefix: vec![0xaa],
            coinbase_suffix: vec![0xbb],
            merkle_branch: vec![],
        };
        let solution = SubmitSolution {
            extranonce2_le: [5, 6, 7, 8],
            ntime: 0x665544cc,
            nonce: 9,
        };
        let verified = verify_share(&template, [1, 2, 3, 4], &solution).unwrap();
        assert_eq!(verified.header.len(), 84);
        assert!(!verified.is_block_candidate);
    }

    #[test]
    fn verifies_share_against_assigned_difficulty_target() {
        let template = WorkTemplate {
            job_id: "job1".to_owned(),
            version: 0x20000000,
            prev: [0; 32],
            time: 0x665544cc,
            bits: 0x207fffff,
            share_target: [0xff; 32],
            network_target: [0; 32],
            coinbase_prefix: vec![0xaa],
            coinbase_suffix: vec![0xbb],
            merkle_branch: vec![],
        };
        let solution = SubmitSolution {
            extranonce2_le: [5, 6, 7, 8],
            ntime: 0x665544cc,
            nonce: 9,
        };
        verify_share_with_difficulty(&template, [1, 2, 3, 4], &solution, 1.0).unwrap();

        assert!(matches!(
            verify_share_with_difficulty(&template, [1, 2, 3, 4], &solution, u64::MAX as f64),
            Err(ConsensusError::LowDifficultyShare)
        ));
    }

    #[test]
    fn rejects_low_difficulty_share() {
        let template = WorkTemplate {
            job_id: "job1".to_owned(),
            version: 0x20000000,
            prev: [0; 32],
            time: 0x665544cc,
            bits: 0x207fffff,
            share_target: [0; 32],
            network_target: [0; 32],
            coinbase_prefix: vec![0xaa],
            coinbase_suffix: vec![0xbb],
            merkle_branch: vec![],
        };
        let solution = SubmitSolution {
            extranonce2_le: [5, 6, 7, 8],
            ntime: 0x665544cc,
            nonce: 9,
        };
        assert!(matches!(
            verify_share(&template, [1, 2, 3, 4], &solution),
            Err(ConsensusError::LowDifficultyShare)
        ));
    }
}
