use {
    ark_bn254::Fr,
    ark_ff::PrimeField,
    ark_r1cs_std::{alloc::AllocVar, eq::EqGadget, fields::fp::FpVar},
    ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError},
    northstar_transaction_proof::{replay, ReplayWitnessV1, VmRowV1},
    northstar_zk_types::ErStepPublicInputsV1,
    solana_sbpf::ebpf,
};

/// Fixture-specific Groth16 harness, not a reusable SBPF transition circuit.
///
/// `replay` validates the witness during synthesis and setup fixes those values as constants.
/// Soundness therefore depends on native replay and this verifying key must not be deployed.
#[derive(Clone, Debug)]
pub struct SbpfExecutionTableCircuitV1 {
    pub public: ErStepPublicInputsV1,
    pub witness: ReplayWitnessV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionTableMetricsV1 {
    pub rows: usize,
    pub opcodes: Vec<u8>,
    pub alu_rows: usize,
    pub branch_rows: usize,
    pub load_rows: usize,
    pub store_rows: usize,
    pub call_rows: usize,
    pub exit_rows: usize,
    pub syscalls: usize,
}

pub fn execution_table_metrics(witness: &ReplayWitnessV1) -> ExecutionTableMetricsV1 {
    let mut opcodes = Vec::new();
    let mut metrics = ExecutionTableMetricsV1 {
        rows: witness.vm_rows.len(),
        opcodes: Vec::new(),
        alu_rows: 0,
        branch_rows: 0,
        load_rows: 0,
        store_rows: 0,
        call_rows: 0,
        exit_rows: 0,
        syscalls: 0,
    };
    for row in &witness.vm_rows {
        let opcode = ebpf::get_insn(&row.instruction, 0).opc;
        opcodes.push(opcode);
        let class = opcode & 0x07;
        let operation = opcode & 0xf0;
        match class {
            1 => metrics.load_rows = metrics.load_rows.saturating_add(1),
            2 | 3 => metrics.store_rows = metrics.store_rows.saturating_add(1),
            4 | 7 => metrics.alu_rows = metrics.alu_rows.saturating_add(1),
            5 | 6 if operation == 0x80 => {
                metrics.call_rows = metrics.call_rows.saturating_add(1);
            }
            5 | 6 if operation == 0x90 => {
                metrics.exit_rows = metrics.exit_rows.saturating_add(1);
            }
            5 | 6 => metrics.branch_rows = metrics.branch_rows.saturating_add(1),
            _ => {}
        }
        if row.syscall_key != 0 {
            metrics.syscalls = metrics.syscalls.saturating_add(1);
        }
    }
    opcodes.sort_unstable();
    opcodes.dedup();
    metrics.opcodes = opcodes;
    metrics
}

impl ConstraintSynthesizer<Fr> for SbpfExecutionTableCircuitV1 {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let derived = replay(&self.witness).map_err(|_| SynthesisError::Unsatisfiable)?;
        if derived != self.public {
            return Err(SynthesisError::Unsatisfiable);
        }
        let public_values = self
            .public
            .to_array()
            .map(|value| Fr::from_be_bytes_mod_order(&value));
        let public = public_values
            .iter()
            .map(|value| FpVar::new_input(cs.clone(), || Ok(*value)))
            .collect::<Result<Vec<_>, _>>()?;
        let derived_values = derived
            .to_array()
            .map(|value| Fr::from_be_bytes_mod_order(&value));
        for (input, expected) in public.iter().zip(derived_values) {
            input.enforce_equal(&FpVar::Constant(expected))?;
        }
        constrain_rows(cs, &self.witness.vm_rows)?;
        Ok(())
    }
}

fn constrain_rows(cs: ConstraintSystemRef<Fr>, rows: &[VmRowV1]) -> Result<(), SynthesisError> {
    for row in rows {
        let instruction = ebpf::get_insn(&row.instruction, 0);
        let opcode = FpVar::new_witness(cs.clone(), || Ok(Fr::from(instruction.opc)))?;
        opcode.enforce_equal(&FpVar::Constant(Fr::from(instruction.opc)))?;
        let pc = FpVar::new_witness(cs.clone(), || Ok(Fr::from(row.registers[11])))?;
        pc.enforce_equal(&FpVar::Constant(Fr::from(row.registers[11])))?;
        for register in row.registers[..11].iter() {
            let value = FpVar::new_witness(cs.clone(), || Ok(Fr::from(*register)))?;
            value.enforce_equal(&FpVar::Constant(Fr::from(*register)))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{prove, setup},
        ark_groth16::{prepare_verifying_key, Groth16},
        ark_relations::r1cs::ConstraintSystem,
        ark_std::rand::SeedableRng,
        northstar_transaction_proof::fixture::build_replay_witness_v1,
        rand_chacha::ChaCha20Rng,
    };

    fn circuit() -> SbpfExecutionTableCircuitV1 {
        let witness = build_replay_witness_v1().unwrap();
        let public = replay(&witness).unwrap();
        SbpfExecutionTableCircuitV1 { public, witness }
    }

    #[test]
    fn table_uses_pinned_sbpf_decoder_and_is_satisfied() {
        let circuit = circuit();
        let metrics = execution_table_metrics(&circuit.witness);
        assert_eq!(metrics.rows, 208);
        assert_eq!(metrics.syscalls, 1);
        let cs = ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap());
        assert_eq!(cs.num_instance_variables(), 9);
    }

    #[test]
    fn mutation_rejects_before_proof() {
        let mut circuit = circuit();
        circuit.witness.vm_rows[0].registers[0] ^= 1;
        let cs = ConstraintSystem::<Fr>::new_ref();
        assert!(circuit.generate_constraints(cs).is_err());
    }

    #[test]
    fn generates_and_verifies_real_custom_groth16_proof() {
        let circuit = circuit();
        let mut setup_rng = ChaCha20Rng::from_seed([31; 32]);
        let proving_key = setup(circuit.clone(), &mut setup_rng).unwrap();
        let mut proof_rng = ChaCha20Rng::from_seed([32; 32]);
        let proof = prove(&proving_key, circuit.clone(), &mut proof_rng).unwrap();
        let prepared = prepare_verifying_key(&proving_key.vk);
        let inputs = circuit
            .public
            .to_array()
            .map(|value| Fr::from_be_bytes_mod_order(&value));
        assert!(Groth16::<ark_bn254::Bn254>::verify_proof(&prepared, &proof, &inputs).unwrap());
    }
}
