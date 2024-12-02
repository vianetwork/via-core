use circuit_definitions::{
    ethereum_types::U256,
    snark_wrapper::franklin_crypto::bellman::{
        bn256::{Bn256, Fr},
        plonk::better_better_cs::{cs::Circuit, proof::Proof},
        CurveAffine, Engine, PrimeField, PrimeFieldRepr,
    },
};

/// Transforms a U256 element into a prime field element.
fn u256_to_scalar<F: PrimeField>(el: &U256) -> F
where
    F::Repr: PrimeFieldRepr + Default,
{
    let mut bytes = [0u8; 32];
    el.to_big_endian(&mut bytes);

    let mut repr = F::Repr::default();
    repr.read_be(&bytes[..])
        .expect("Failed to read bytes into field representation");

    F::from_repr(repr).expect("Failed to convert U256 to scalar")
}

/// Transforms a point represented as a pair of U256 into its affine representation.
fn deserialize_g1(point: (U256, U256)) -> <Bn256 as Engine>::G1Affine {
    if point == (U256::zero(), U256::zero()) {
        return <Bn256 as Engine>::G1Affine::zero();
    }

    let x_scalar = u256_to_scalar(&point.0);
    let y_scalar = u256_to_scalar(&point.1);

    <Bn256 as Engine>::G1Affine::from_xy_unchecked(x_scalar, y_scalar)
}

/// Transforms a field element represented as U256 into the field representation.
fn deserialize_fe(felt: U256) -> Fr {
    u256_to_scalar(&felt)
}

/// Deserializes a proof from a vector of U256 elements.
pub fn deserialize_proof<T: Circuit<Bn256>>(mut proof: Vec<U256>) -> Proof<Bn256, T> {
    let opening_proof_at_z_omega = {
        let y = proof
            .pop()
            .expect("Missing y-coordinate for opening_proof_at_z_omega");
        let x = proof
            .pop()
            .expect("Missing x-coordinate for opening_proof_at_z_omega");
        deserialize_g1((x, y))
    };

    let opening_proof_at_z = {
        let y = proof
            .pop()
            .expect("Missing y-coordinate for opening_proof_at_z");
        let x = proof
            .pop()
            .expect("Missing x-coordinate for opening_proof_at_z");
        deserialize_g1((x, y))
    };

    let linearization_poly_opening_at_z = deserialize_fe(
        proof
            .pop()
            .expect("Missing linearization_poly_opening_at_z"),
    );
    let quotient_poly_opening_at_z =
        deserialize_fe(proof.pop().expect("Missing quotient_poly_opening_at_z"));
    let lookup_table_type_poly_opening_at_z = deserialize_fe(
        proof
            .pop()
            .expect("Missing lookup_table_type_poly_opening_at_z"),
    );
    let lookup_selector_poly_opening_at_z = deserialize_fe(
        proof
            .pop()
            .expect("Missing lookup_selector_poly_opening_at_z"),
    );
    let lookup_t_poly_opening_at_z_omega = deserialize_fe(
        proof
            .pop()
            .expect("Missing lookup_t_poly_opening_at_z_omega"),
    );
    let lookup_t_poly_opening_at_z =
        deserialize_fe(proof.pop().expect("Missing lookup_t_poly_opening_at_z"));
    let lookup_grand_product_opening_at_z_omega = deserialize_fe(
        proof
            .pop()
            .expect("Missing lookup_grand_product_opening_at_z_omega"),
    );
    let lookup_s_poly_opening_at_z_omega = deserialize_fe(
        proof
            .pop()
            .expect("Missing lookup_s_poly_opening_at_z_omega"),
    );
    let copy_permutation_grand_product_opening_at_z_omega = deserialize_fe(
        proof
            .pop()
            .expect("Missing copy_permutation_grand_product_opening_at_z_omega"),
    );

    let mut copy_permutation_polys_openings_at_z = vec![];
    for _ in 0..3 {
        copy_permutation_polys_openings_at_z.push(deserialize_fe(
            proof
                .pop()
                .expect("Missing copy_permutation_polys_openings_at_z"),
        ));
    }
    copy_permutation_polys_openings_at_z.reverse();

    let gate_selectors_openings_at_z = vec![(
        0_usize,
        deserialize_fe(proof.pop().expect("Missing gate_selectors_openings_at_z")),
    )];

    let state_polys_openings_at_dilations = {
        let fe = deserialize_fe(
            proof
                .pop()
                .expect("Missing state_polys_openings_at_dilations"),
        );
        vec![(1_usize, 3_usize, fe)]
    };

    let mut state_polys_openings_at_z = vec![];
    for _ in 0..4 {
        state_polys_openings_at_z.push(deserialize_fe(
            proof.pop().expect("Missing state_polys_openings_at_z"),
        ));
    }
    state_polys_openings_at_z.reverse();

    let mut quotient_poly_parts_commitments = vec![];
    for _ in 0..4 {
        let y = proof
            .pop()
            .expect("Missing y-coordinate for quotient_poly_parts_commitments");
        let x = proof
            .pop()
            .expect("Missing x-coordinate for quotient_poly_parts_commitments");
        quotient_poly_parts_commitments.push(deserialize_g1((x, y)));
    }
    quotient_poly_parts_commitments.reverse();

    let lookup_grand_product_commitment = {
        let y = proof
            .pop()
            .expect("Missing y-coordinate for lookup_grand_product_commitment");
        let x = proof
            .pop()
            .expect("Missing x-coordinate for lookup_grand_product_commitment");
        deserialize_g1((x, y))
    };

    let lookup_s_poly_commitment = {
        let y = proof
            .pop()
            .expect("Missing y-coordinate for lookup_s_poly_commitment");
        let x = proof
            .pop()
            .expect("Missing x-coordinate for lookup_s_poly_commitment");
        deserialize_g1((x, y))
    };

    let copy_permutation_grand_product_commitment = {
        let y = proof
            .pop()
            .expect("Missing y-coordinate for copy_permutation_grand_product_commitment");
        let x = proof
            .pop()
            .expect("Missing x-coordinate for copy_permutation_grand_product_commitment");
        deserialize_g1((x, y))
    };

    let mut state_polys_commitments = vec![];
    for _ in 0..4 {
        let y = proof
            .pop()
            .expect("Missing y-coordinate for state_polys_commitments");
        let x = proof
            .pop()
            .expect("Missing x-coordinate for state_polys_commitments");
        state_polys_commitments.push(deserialize_g1((x, y)));
    }
    state_polys_commitments.reverse();

    let mut proof_obj: Proof<Bn256, T> = Proof::empty();

    proof_obj.state_polys_commitments = state_polys_commitments;
    proof_obj.copy_permutation_grand_product_commitment = copy_permutation_grand_product_commitment;
    proof_obj.lookup_s_poly_commitment = Some(lookup_s_poly_commitment);
    proof_obj.lookup_grand_product_commitment = Some(lookup_grand_product_commitment);
    proof_obj.quotient_poly_parts_commitments = quotient_poly_parts_commitments;
    proof_obj.state_polys_openings_at_z = state_polys_openings_at_z;
    proof_obj.state_polys_openings_at_dilations = state_polys_openings_at_dilations;
    proof_obj.gate_selectors_openings_at_z = gate_selectors_openings_at_z;
    proof_obj.copy_permutation_polys_openings_at_z = copy_permutation_polys_openings_at_z;
    proof_obj.copy_permutation_grand_product_opening_at_z_omega =
        copy_permutation_grand_product_opening_at_z_omega;
    proof_obj.lookup_s_poly_opening_at_z_omega = Some(lookup_s_poly_opening_at_z_omega);
    proof_obj.lookup_grand_product_opening_at_z_omega =
        Some(lookup_grand_product_opening_at_z_omega);
    proof_obj.lookup_t_poly_opening_at_z = Some(lookup_t_poly_opening_at_z);
    proof_obj.lookup_t_poly_opening_at_z_omega = Some(lookup_t_poly_opening_at_z_omega);
    proof_obj.lookup_selector_poly_opening_at_z = Some(lookup_selector_poly_opening_at_z);
    proof_obj.lookup_table_type_poly_opening_at_z = Some(lookup_table_type_poly_opening_at_z);
    proof_obj.quotient_poly_opening_at_z = quotient_poly_opening_at_z;
    proof_obj.linearization_poly_opening_at_z = linearization_poly_opening_at_z;
    proof_obj.opening_proof_at_z = opening_proof_at_z;
    proof_obj.opening_proof_at_z_omega = opening_proof_at_z_omega;

    proof_obj
}
