//! Circom Poseidon constants and encodings shared by replay and proof circuits.
pub use light_poseidon::PoseidonError;
use {
    ark_bn254::Fr,
    light_poseidon::{Poseidon, PoseidonHasher},
};

pub struct PoseidonParameters {
    pub ark: Vec<Fr>,
    pub mds: Vec<Vec<Fr>>,
    pub full_rounds: usize,
    pub partial_rounds: usize,
    pub width: usize,
}

pub fn parameters(width: u8) -> Result<PoseidonParameters, PoseidonError> {
    let params = light_poseidon::parameters::bn254_x5::get_poseidon_parameters::<Fr>(width)?;
    Ok(PoseidonParameters {
        ark: params.ark,
        mds: params.mds,
        full_rounds: params.full_rounds,
        partial_rounds: params.partial_rounds,
        width: params.width,
    })
}

pub fn hash(inputs: &[Fr]) -> Result<Fr, PoseidonError> {
    Poseidon::<Fr>::new_circom(inputs.len())?.hash(inputs)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        ark_ff::{BigInteger, PrimeField},
    };

    #[test]
    fn circom_hash_preserves_pre_upgrade_vector() {
        let inputs = [
            Fr::from_be_bytes_mod_order(&[1; 32]),
            Fr::from_be_bytes_mod_order(&[2; 32]),
        ];
        assert_eq!(
            hash(&inputs).unwrap().into_bigint().to_bytes_be(),
            [
                13, 84, 225, 147, 143, 138, 140, 28, 125, 235, 94, 3, 85, 242, 99, 25, 32, 123,
                132, 254, 156, 162, 206, 27, 38, 231, 53, 200, 41, 130, 25, 144,
            ]
        );
    }

    #[test]
    fn hash_preserves_modulus_boundaries() {
        for value in [Fr::from(0u64), Fr::from(1u64), -Fr::from(1u64)] {
            let expected = Poseidon::<Fr>::new_circom(2)
                .unwrap()
                .hash(&[value, value])
                .unwrap();
            assert_eq!(hash(&[value, value]).unwrap(), expected);
        }
    }
}
