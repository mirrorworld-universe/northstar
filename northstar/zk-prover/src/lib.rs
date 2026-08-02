use {
    ark_bn254::{Bn254, Fr, G1Affine, G2Affine},
    ark_ff::{BigInteger, PrimeField},
    ark_groth16::{Groth16, Proof, ProvingKey, VerifyingKey},
    ark_r1cs_std::{
        alloc::AllocVar,
        boolean::Boolean,
        convert::ToBitsGadget,
        eq::EqGadget,
        fields::{fp::FpVar, FieldVar},
    },
    ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError},
    ark_serialize::{CanonicalSerialize, Compress, SerializationError},
    ark_std::rand::Rng,
    light_poseidon::{parameters::bn254_x5, Poseidon, PoseidonHasher, PoseidonParameters},
    northstar_zk_types::{ErStepPublicInputsV1, FrBytes, Groth16ProofRaw},
};

pub const ACCOUNT_TREE_DEPTH: usize = 32;
pub const ER_STEP_PROOF_KIND_ONE_ACCOUNT: u8 = 1;
pub const ER_STEP_PROOF_VERSION_V1: u8 = 1;

const ACCOUNT_LEAF_TAG: u64 = 1;
const EFFECT_LEAF_TAG: u64 = 2;
const EFFECT_ROOT_TAG: u64 = 3;
const TX_EFFECT_TAG: u64 = 4;

#[derive(Debug)]
pub enum ProverError {
    Poseidon(light_poseidon::PoseidonError),
    Synthesis(SynthesisError),
    Serialization(SerializationError),
}

impl From<light_poseidon::PoseidonError> for ProverError {
    fn from(value: light_poseidon::PoseidonError) -> Self {
        Self::Poseidon(value)
    }
}

impl From<SynthesisError> for ProverError {
    fn from(value: SynthesisError) -> Self {
        Self::Synthesis(value)
    }
}

impl From<SerializationError> for ProverError {
    fn from(value: SerializationError) -> Self {
        Self::Serialization(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountStateV1 {
    pub account_id: Fr,
    pub owner: Fr,
    pub lamports: u64,
    pub data_root: Fr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OneAccountTransitionWitnessV1 {
    pub pre: AccountStateV1,
    pub post: AccountStateV1,
    pub siblings: [Fr; ACCOUNT_TREE_DEPTH],
    /// Little-endian path bits: `true` means current node is right child.
    pub path_bits: [bool; ACCOUNT_TREE_DEPTH],
}

#[derive(Clone, Debug)]
pub struct OneAccountTransitionCircuitV1 {
    pub public: ErStepPublicInputsV1,
    pub witness: OneAccountTransitionWitnessV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Groth16VerifyingKeyRaw {
    pub alpha_g1: [u8; 64],
    pub beta_g2: [u8; 128],
    pub gamma_g2: [u8; 128],
    pub delta_g2: [u8; 128],
    pub ic: Vec<[u8; 64]>,
}

fn poseidon_parameters() -> Result<PoseidonParameters<Fr>, ProverError> {
    Ok(bn254_x5::get_poseidon_parameters::<Fr>(3)?)
}

fn poseidon2_native(left: Fr, right: Fr) -> Result<Fr, ProverError> {
    Ok(Poseidon::<Fr>::new_circom(2)?.hash(&[left, right])?)
}

fn poseidon_fold_native(tag: u64, values: &[Fr]) -> Result<Fr, ProverError> {
    let mut accumulator = Fr::from(tag);
    for value in values {
        accumulator = poseidon2_native(accumulator, *value)?;
    }
    Ok(accumulator)
}

// `FpVar` operators build modular BN254 constraints; wraparound is the field definition.
#[allow(clippy::arithmetic_side_effects)]
fn poseidon2_var(
    left: &FpVar<Fr>,
    right: &FpVar<Fr>,
    params: &PoseidonParameters<Fr>,
) -> Result<FpVar<Fr>, SynthesisError> {
    let mut state = [FpVar::zero(), left.clone(), right.clone()];
    let all_rounds = params
        .full_rounds
        .checked_add(params.partial_rounds)
        .ok_or(SynthesisError::Unsatisfiable)?;
    let half_full_rounds = params.full_rounds / 2;
    let partial_rounds_end = half_full_rounds
        .checked_add(params.partial_rounds)
        .ok_or(SynthesisError::Unsatisfiable)?;

    for (round, round_constants) in params
        .ark
        .chunks_exact(params.width)
        .take(all_rounds)
        .enumerate()
    {
        for (index, value) in state.iter_mut().enumerate() {
            *value += round_constants[index];
        }

        let full_round = round < half_full_rounds || round >= partial_rounds_end;
        if full_round {
            for value in &mut state {
                *value = value.square()?.square()? * value.clone();
            }
        } else {
            state[0] = state[0].square()?.square()? * state[0].clone();
        }

        let previous = state.clone();
        for (row, output) in state.iter_mut().enumerate() {
            *output = FpVar::zero();
            for (column, input) in previous.iter().enumerate() {
                *output += input * params.mds[row][column];
            }
        }
    }

    Ok(state[0].clone())
}

fn poseidon_fold_var(
    tag: u64,
    values: &[FpVar<Fr>],
    params: &PoseidonParameters<Fr>,
) -> Result<FpVar<Fr>, SynthesisError> {
    let mut accumulator = FpVar::Constant(Fr::from(tag));
    for value in values {
        accumulator = poseidon2_var(&accumulator, value, params)?;
    }
    Ok(accumulator)
}

fn account_leaf_native(account: &AccountStateV1) -> Result<Fr, ProverError> {
    poseidon_fold_native(
        ACCOUNT_LEAF_TAG,
        &[
            account.account_id,
            account.owner,
            Fr::from(account.lamports),
            account.data_root,
        ],
    )
}

fn effect_root_native(pre: &AccountStateV1, post: &AccountStateV1) -> Result<Fr, ProverError> {
    let effect_leaf = poseidon_fold_native(
        EFFECT_LEAF_TAG,
        &[
            pre.account_id,
            pre.owner,
            Fr::from(pre.lamports),
            Fr::from(post.lamports),
            pre.data_root,
            post.data_root,
        ],
    )?;
    poseidon_fold_native(EFFECT_ROOT_TAG, &[effect_leaf])
}

fn merkle_root_native(
    leaf: Fr,
    siblings: &[Fr; ACCOUNT_TREE_DEPTH],
    path_bits: &[bool; ACCOUNT_TREE_DEPTH],
) -> Result<Fr, ProverError> {
    let mut current = leaf;
    for (sibling, current_is_right) in siblings.iter().zip(path_bits) {
        current = if *current_is_right {
            poseidon2_native(*sibling, current)?
        } else {
            poseidon2_native(current, *sibling)?
        };
    }
    Ok(current)
}

fn fr_from_bytes(value: FrBytes) -> Fr {
    Fr::from_be_bytes_mod_order(value.as_bytes())
}

pub fn fr_to_bytes(value: Fr) -> FrBytes {
    let encoded = value.into_bigint().to_bytes_be();
    let mut bytes = [0; 32];
    let padding = 32usize
        .checked_sub(encoded.len())
        .expect("BN254 scalar encoding fits 32 bytes");
    bytes[padding..].copy_from_slice(&encoded);
    FrBytes::new(bytes).expect("arkworks emitted a canonical BN254 scalar")
}

pub fn derive_public_inputs_v1(
    session_context: Fr,
    er_slot: u64,
    step_index: u64,
    readonly_l1_root: Fr,
    witness: &OneAccountTransitionWitnessV1,
) -> Result<ErStepPublicInputsV1, ProverError> {
    let domain =
        FrBytes::er_step_domain_v1(ER_STEP_PROOF_KIND_ONE_ACCOUNT, ER_STEP_PROOF_VERSION_V1);
    let effect_root = effect_root_native(&witness.pre, &witness.post)?;
    let pre_state_root = merkle_root_native(
        account_leaf_native(&witness.pre)?,
        &witness.siblings,
        &witness.path_bits,
    )?;
    let post_state_root = merkle_root_native(
        account_leaf_native(&witness.post)?,
        &witness.siblings,
        &witness.path_bits,
    )?;
    let slot_step =
        Fr::from_be_bytes_mod_order(FrBytes::from_u64_pair(er_slot, step_index).as_bytes());
    let tx_effect_root = poseidon_fold_native(
        TX_EFFECT_TAG,
        &[
            fr_from_bytes(domain),
            session_context,
            slot_step,
            readonly_l1_root,
            effect_root,
        ],
    )?;

    Ok(ErStepPublicInputsV1 {
        domain,
        session_context: fr_to_bytes(session_context),
        slot_step: FrBytes::from_u64_pair(er_slot, step_index),
        pre_state_root: fr_to_bytes(pre_state_root),
        post_state_root: fr_to_bytes(post_state_root),
        tx_effect_root: fr_to_bytes(tx_effect_root),
        readonly_l1_root: fr_to_bytes(readonly_l1_root),
        settlement_effect_root: fr_to_bytes(effect_root),
    })
}

impl ConstraintSynthesizer<Fr> for OneAccountTransitionCircuitV1 {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let params = poseidon_parameters().map_err(|_| SynthesisError::Unsatisfiable)?;
        let public_values = self
            .public
            .to_array()
            .map(|value| Fr::from_be_bytes_mod_order(&value));
        let public: Vec<FpVar<Fr>> = public_values
            .iter()
            .map(|value| FpVar::new_input(cs.clone(), || Ok(*value)))
            .collect::<Result<_, _>>()?;

        public[0].enforce_equal(&FpVar::Constant(fr_from_bytes(FrBytes::er_step_domain_v1(
            ER_STEP_PROOF_KIND_ONE_ACCOUNT,
            ER_STEP_PROOF_VERSION_V1,
        ))))?;

        let account_id = FpVar::new_witness(cs.clone(), || Ok(self.witness.pre.account_id))?;
        let owner = FpVar::new_witness(cs.clone(), || Ok(self.witness.pre.owner))?;
        let post_account_id = FpVar::new_witness(cs.clone(), || Ok(self.witness.post.account_id))?;
        let post_owner = FpVar::new_witness(cs.clone(), || Ok(self.witness.post.owner))?;
        account_id.enforce_equal(&post_account_id)?;
        owner.enforce_equal(&post_owner)?;

        let pre_lamports =
            FpVar::new_witness(cs.clone(), || Ok(Fr::from(self.witness.pre.lamports)))?;
        let post_lamports =
            FpVar::new_witness(cs.clone(), || Ok(Fr::from(self.witness.post.lamports)))?;
        let pre_data_root = FpVar::new_witness(cs.clone(), || Ok(self.witness.pre.data_root))?;
        let post_data_root = FpVar::new_witness(cs.clone(), || Ok(self.witness.post.data_root))?;
        for lamports in [&pre_lamports, &post_lamports] {
            for high_bit in lamports.to_bits_le()?.iter().skip(64) {
                high_bit.enforce_equal(&Boolean::constant(false))?;
            }
        }

        let pre_leaf = poseidon_fold_var(
            ACCOUNT_LEAF_TAG,
            &[
                account_id.clone(),
                owner.clone(),
                pre_lamports.clone(),
                pre_data_root.clone(),
            ],
            &params,
        )?;
        let post_leaf = poseidon_fold_var(
            ACCOUNT_LEAF_TAG,
            &[
                account_id.clone(),
                owner.clone(),
                post_lamports.clone(),
                post_data_root.clone(),
            ],
            &params,
        )?;

        let siblings: Vec<FpVar<Fr>> = self
            .witness
            .siblings
            .iter()
            .map(|value| FpVar::new_witness(cs.clone(), || Ok(*value)))
            .collect::<Result<_, _>>()?;
        let path_bits: Vec<Boolean<Fr>> = self
            .witness
            .path_bits
            .iter()
            .map(|value| Boolean::new_witness(cs.clone(), || Ok(*value)))
            .collect::<Result<_, _>>()?;

        let mut pre_root = pre_leaf;
        let mut post_root = post_leaf;
        for (sibling, current_is_right) in siblings.iter().zip(&path_bits) {
            let pre_left = current_is_right.select(sibling, &pre_root)?;
            let pre_right = current_is_right.select(&pre_root, sibling)?;
            pre_root = poseidon2_var(&pre_left, &pre_right, &params)?;

            let post_left = current_is_right.select(sibling, &post_root)?;
            let post_right = current_is_right.select(&post_root, sibling)?;
            post_root = poseidon2_var(&post_left, &post_right, &params)?;
        }
        pre_root.enforce_equal(&public[3])?;
        post_root.enforce_equal(&public[4])?;

        let effect_leaf = poseidon_fold_var(
            EFFECT_LEAF_TAG,
            &[
                account_id,
                owner,
                pre_lamports,
                post_lamports,
                pre_data_root,
                post_data_root,
            ],
            &params,
        )?;
        let effect_root = poseidon_fold_var(EFFECT_ROOT_TAG, &[effect_leaf], &params)?;
        effect_root.enforce_equal(&public[7])?;

        let tx_effect_root = poseidon_fold_var(
            TX_EFFECT_TAG,
            &[
                public[0].clone(),
                public[1].clone(),
                public[2].clone(),
                public[6].clone(),
                public[7].clone(),
            ],
            &params,
        )?;
        tx_effect_root.enforce_equal(&public[5])?;
        Ok(())
    }
}

pub fn setup<R: Rng>(
    circuit: OneAccountTransitionCircuitV1,
    rng: &mut R,
) -> Result<ProvingKey<Bn254>, ProverError> {
    Ok(Groth16::<Bn254>::generate_random_parameters_with_reduction(
        circuit, rng,
    )?)
}

pub fn prove<R: Rng>(
    proving_key: &ProvingKey<Bn254>,
    circuit: OneAccountTransitionCircuitV1,
    rng: &mut R,
) -> Result<Proof<Bn254>, ProverError> {
    Ok(Groth16::<Bn254>::create_random_proof_with_reduction(
        circuit,
        proving_key,
        rng,
    )?)
}

fn reverse_chunks<const CHUNK: usize, const TOTAL: usize>(bytes: &[u8; TOTAL]) -> [u8; TOTAL] {
    let mut output = [0; TOTAL];
    for (source, destination) in bytes
        .chunks_exact(CHUNK)
        .zip(output.chunks_exact_mut(CHUNK))
    {
        for (index, byte) in source.iter().rev().enumerate() {
            destination[index] = *byte;
        }
    }
    output
}

fn g1_to_be(point: &G1Affine) -> Result<[u8; 64], ProverError> {
    let mut serialized = [0; 65];
    point.serialize_with_mode(&mut serialized[..], Compress::No)?;
    let coordinates: [u8; 64] = serialized[..64]
        .try_into()
        .expect("fixed coordinate length");
    Ok(reverse_chunks::<32, 64>(&coordinates))
}

fn g2_to_be(point: &G2Affine) -> Result<[u8; 128], ProverError> {
    let mut serialized = [0; 129];
    point.serialize_with_mode(&mut serialized[..], Compress::No)?;
    let coordinates: [u8; 128] = serialized[..128]
        .try_into()
        .expect("fixed coordinate length");
    Ok(reverse_chunks::<64, 128>(&coordinates))
}

pub fn proof_to_solana(proof: &Proof<Bn254>) -> Result<Groth16ProofRaw, ProverError> {
    Ok(Groth16ProofRaw {
        a: g1_to_be(&core::ops::Neg::neg(proof.a))?,
        b: g2_to_be(&proof.b)?,
        c: g1_to_be(&proof.c)?,
    })
}

pub fn verifying_key_to_solana(
    verifying_key: &VerifyingKey<Bn254>,
) -> Result<Groth16VerifyingKeyRaw, ProverError> {
    Ok(Groth16VerifyingKeyRaw {
        alpha_g1: g1_to_be(&verifying_key.alpha_g1)?,
        beta_g2: g2_to_be(&verifying_key.beta_g2)?,
        gamma_g2: g2_to_be(&verifying_key.gamma_g2)?,
        delta_g2: g2_to_be(&verifying_key.delta_g2)?,
        ic: verifying_key
            .gamma_abc_g1
            .iter()
            .map(g1_to_be)
            .collect::<Result<_, _>>()?,
    })
}

pub fn sample_witness() -> OneAccountTransitionWitnessV1 {
    let siblings = core::array::from_fn(|index| {
        Fr::from(
            u64::try_from(index)
                .expect("tree depth fits u64")
                .checked_add(100)
                .expect("sample sibling does not overflow"),
        )
    });
    let path_bits = core::array::from_fn(|index| index % 3 == 1);
    OneAccountTransitionWitnessV1 {
        pre: AccountStateV1 {
            account_id: Fr::from(11u64),
            owner: Fr::from(12u64),
            lamports: 1_000_000,
            data_root: Fr::from(13u64),
        },
        post: AccountStateV1 {
            account_id: Fr::from(11u64),
            owner: Fr::from(12u64),
            lamports: 999_000,
            data_root: Fr::from(14u64),
        },
        siblings,
        path_bits,
    }
}

pub fn sample_circuit() -> Result<OneAccountTransitionCircuitV1, ProverError> {
    let witness = sample_witness();
    let public = derive_public_inputs_v1(Fr::from(21u64), 42, 7, Fr::from(22u64), &witness)?;
    Ok(OneAccountTransitionCircuitV1 { public, witness })
}

pub fn constraint_count(circuit: OneAccountTransitionCircuitV1) -> Result<usize, ProverError> {
    let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
    circuit.generate_constraints(cs.clone())?;
    cs.finalize();
    Ok(cs.num_constraints())
}

#[cfg(test)]
mod tests {
    use {
        super::*, ark_groth16::prepare_verifying_key, ark_relations::r1cs::ConstraintSystem,
        ark_std::rand::SeedableRng, rand_chacha::ChaCha20Rng,
    };

    #[test]
    fn native_and_constrained_transition_match() {
        let circuit = sample_circuit().unwrap();
        let cs = ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap());
        assert_eq!(cs.num_instance_variables(), 9);
        assert!(cs.num_constraints() < 100_000);
    }

    #[test]
    fn changing_public_effect_root_breaks_constraints() {
        let mut circuit = sample_circuit().unwrap();
        circuit.public.settlement_effect_root = FrBytes::from_u64(9);
        let cs = ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }

    #[test]
    fn generates_and_verifies_proof() {
        let circuit = sample_circuit().unwrap();
        let mut setup_rng = ChaCha20Rng::from_seed([7; 32]);
        let proving_key = setup(circuit.clone(), &mut setup_rng).unwrap();
        let mut proof_rng = ChaCha20Rng::from_seed([8; 32]);
        let proof = prove(&proving_key, circuit.clone(), &mut proof_rng).unwrap();
        let prepared = prepare_verifying_key(&proving_key.vk);
        let inputs = circuit
            .public
            .to_array()
            .map(|value| Fr::from_be_bytes_mod_order(&value));
        assert!(Groth16::<Bn254>::verify_proof(&prepared, &proof, &inputs).unwrap());
        assert_eq!(proof_to_solana(&proof).unwrap().to_bytes().len(), 256);
        assert_eq!(
            verifying_key_to_solana(&proving_key.vk).unwrap().ic.len(),
            9
        );
    }

    #[test]
    fn poseidon_gadget_matches_light_poseidon() {
        let left = Fr::from(31u64);
        let right = Fr::from(32u64);
        let expected = poseidon2_native(left, right).unwrap();
        let cs = ConstraintSystem::<Fr>::new_ref();
        let left_var = FpVar::new_witness(cs.clone(), || Ok(left)).unwrap();
        let right_var = FpVar::new_witness(cs.clone(), || Ok(right)).unwrap();
        let actual = poseidon2_var(&left_var, &right_var, &poseidon_parameters().unwrap()).unwrap();
        actual.enforce_equal(&FpVar::Constant(expected)).unwrap();
        assert!(cs.is_satisfied().unwrap());
    }
}
