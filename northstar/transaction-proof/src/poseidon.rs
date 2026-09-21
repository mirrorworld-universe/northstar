//! Keep Circom Poseidon constants and encodings stable across Arkworks versions.
pub use light_poseidon::PoseidonError;
use {
    ark_bn254::Fr,
    ark_ff::{BigInteger, PrimeField},
    light_poseidon::{Poseidon, PoseidonHasher},
    poseidon_ark_bn254::Fr as LegacyFr,
    poseidon_ark_ff::{BigInteger as _, PrimeField as _},
};

pub struct PoseidonParameters {
    pub ark: Vec<Fr>,
    pub mds: Vec<Vec<Fr>>,
    pub full_rounds: usize,
    pub partial_rounds: usize,
    pub width: usize,
}

fn from_legacy(value: LegacyFr) -> Fr {
    Fr::from_be_bytes_mod_order(&value.into_bigint().to_bytes_be())
}

pub fn parameters(width: u8) -> Result<PoseidonParameters, PoseidonError> {
    let params = light_poseidon::parameters::bn254_x5::get_poseidon_parameters::<LegacyFr>(width)?;
    Ok(PoseidonParameters {
        ark: params.ark.into_iter().map(from_legacy).collect(),
        mds: params
            .mds
            .into_iter()
            .map(|row| row.into_iter().map(from_legacy).collect())
            .collect(),
        full_rounds: params.full_rounds,
        partial_rounds: params.partial_rounds,
        width: params.width,
    })
}

pub fn hash(inputs: &[Fr]) -> Result<Fr, PoseidonError> {
    let inputs = inputs
        .iter()
        .map(|value| LegacyFr::from_be_bytes_mod_order(&value.into_bigint().to_bytes_be()))
        .collect::<Vec<_>>();
    Poseidon::<LegacyFr>::new_circom(inputs.len())?
        .hash(&inputs)
        .map(from_legacy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_bridge_preserves_modulus_boundaries_and_poseidon_outputs() {
        for value in [Fr::from(0u64), Fr::from(1u64), -Fr::from(1u64)] {
            let legacy = LegacyFr::from_be_bytes_mod_order(&value.into_bigint().to_bytes_be());
            assert_eq!(from_legacy(legacy), value);
            let expected = Poseidon::<LegacyFr>::new_circom(2)
                .unwrap()
                .hash(&[legacy, legacy])
                .unwrap();
            assert_eq!(
                hash(&[value, value]).unwrap().into_bigint().to_bytes_be(),
                expected.into_bigint().to_bytes_be()
            );
        }
    }
}
