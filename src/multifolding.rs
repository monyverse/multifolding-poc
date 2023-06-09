use ark_bls12_381::Fr;
use ark_ff::Field;
use ark_std::Zero;

use subroutines::PolyIOP;
use transcript::IOPTranscript;

use crate::ccs::hypercube::BooleanHypercube;
use crate::ccs::{CCCS, CCS, LCCCS};
use crate::espresso::sum_check::structs::IOPProof as SumCheckProof;
use crate::espresso::sum_check::{verifier::interpolate_uni_poly, SumCheck};
use crate::espresso::virtual_polynomial::VPAuxInfo;

use std::marker::PhantomData;

#[derive(Debug)]
pub struct Multifolding {
    pub running_instance: LCCCS,
    pub new_instance: CCCS,
}

impl Multifolding {
    // TODO: This should not take ccs as input. We should move useful CCS methods to multifolding and LCCCS1
    fn prove(
        &self,
        ccs: &CCS,
        z_1: &Vec<Fr>,
        z_2: &Vec<Fr>,
    ) -> (SumCheckProof<Fr>, Vec<Fr>, Vec<Fr>) {
        let mut transcript = IOPTranscript::<Fr>::new(b"multifolding");
        transcript.append_message(b"TMP", b"TMP").unwrap();
        // TODO appends to transcript

        let gamma: Fr = transcript.get_and_append_challenge(b"gamma").unwrap();
        let beta: Vec<Fr> = transcript
            .get_and_append_challenge_vectors(b"beta", ccs.s)
            .unwrap();

        // compute g(x)
        let g = ccs.compute_g(z_1, z_2, gamma, &beta, &self.running_instance.r_x);

        let sc_proof = <PolyIOP<Fr> as SumCheck<Fr>>::prove(&g, &mut transcript).unwrap(); // XXX unwrap

        // Note: The following two "sanity checks" are done for this prototype, in a final version
        // can be removed for efficiency.
        //
        // Sanity check 1: evaluate g(x) over x \in {0,1} (the boolean hypercube), and check that
        // its sum is equal to the extracted_sum from the SumCheck.
        //////////////////////////////////////////////////////////////////////
        let mut g_over_bhc = Fr::zero();
        for x in BooleanHypercube::new(ccs.s) {
            g_over_bhc += g.evaluate(&x).unwrap();
        }

        // note: this is the sum of g(x) over the whole boolean hypercube, not g(r_x_prime)
        let extracted_sum = <PolyIOP<Fr> as SumCheck<Fr>>::extract_sum(&sc_proof);
        assert_eq!(extracted_sum, g_over_bhc);
        // Sanity check 2: expect \sum v_j * gamma^j to be equal to the sum of g(x) over the
        // boolean hypercube (and also equal to the extracted_sum from the SumCheck).
        let mut sum_v_j_gamma = Fr::zero();
        for j in 0..self.running_instance.v.len() {
            let gamma_j = gamma.pow([j as u64]);
            sum_v_j_gamma += self.running_instance.v[j] * gamma_j;
        }
        assert_eq!(g_over_bhc, sum_v_j_gamma);
        assert_eq!(extracted_sum, sum_v_j_gamma);
        //////////////////////////////////////////////////////////////////////

        // get r_x' from the SumCheck used challenge (which inside the SC it comes from the transcript)
        let r_x_prime = sc_proof.point.clone();

        // Compute sigmas and thetas
        let (sigmas, thetas) = ccs.compute_sigmas_and_thetas(z_1, z_2, &r_x_prime);
        (sc_proof, sigmas, thetas)
    }

    fn verify(self, ccs: &CCS, proof: SumCheckProof<Fr>, sigmas: &[Fr], thetas: &[Fr]) {
        let mut transcript = IOPTranscript::<Fr>::new(b"multifolding");
        transcript.append_message(b"TMP", b"TMP").unwrap();
        // TODO appends to transcript

        let gamma: Fr = transcript.get_and_append_challenge(b"gamma").unwrap();
        let beta: Vec<Fr> = transcript
            .get_and_append_challenge_vectors(b"beta", ccs.s)
            .unwrap();

        let vp_aux_info = VPAuxInfo::<Fr> {
            max_degree: ccs.d + 1,
            num_variables: ccs.s,
            phantom: PhantomData::<Fr>,
        };

        // Compute \sum gamma^j u_j
        let mut sum_v_j_gamma = Fr::zero();
        for j in 0..self.running_instance.v.len() {
            let gamma_j = gamma.pow([j as u64]);
            sum_v_j_gamma += self.running_instance.v[j] * gamma_j;
        }

        // verify sumcheck
        let sc_subclaim = <PolyIOP<Fr> as SumCheck<Fr>>::verify(
            sum_v_j_gamma,
            &proof,
            &vp_aux_info,
            &mut transcript,
        )
        .unwrap();

        // Dig into the sumcheck claim and extract the randomness used
        let r_x_prime = sc_subclaim.point.clone();

        // Step 5 from the multifolding verification
        let c = ccs.compute_c_from_sigmas_and_thetas(
            sigmas,
            thetas,
            gamma,
            &beta,
            &self.running_instance.r_x,
            &r_x_prime,
        );
        // check that the g(r_x') from SumCheck proof is equal to the obtained c from sigmas&thetas
        assert_eq!(c, sc_subclaim.expected_evaluation);

        // Sanity check: we can also compute g(r_x') from the proof last evaluation value, and
        // should be equal to the previously obtained values.
        let g_on_rxprime_from_SC_last_eval = interpolate_uni_poly::<Fr>(
            &proof.proofs.last().unwrap().evaluations,
            *r_x_prime.last().unwrap(),
        )
        .unwrap();
        assert_eq!(g_on_rxprime_from_SC_last_eval, c);
        assert_eq!(
            g_on_rxprime_from_SC_last_eval,
            sc_subclaim.expected_evaluation
        );
    }
}

#[cfg(test)]
pub mod test {
    use super::*;
    use crate::ccs::ccs::{get_test_ccs, get_test_z};
    use ark_std::test_rng;
    use ark_std::UniformRand;

    use crate::ccs::pedersen;

    #[test]
    pub fn test_multifolding() {
        let mut rng = test_rng();

        let ccs = get_test_ccs();
        // LCCCS witness
        let z_1 = get_test_z(3);
        // CCS witness
        let z_2 = get_test_z(4);

        // Compute some parts of the input LCCCS instance
        // XXX move to its own structure
        let r_x: Vec<Fr> = (0..ccs.s).map(|_| Fr::rand(&mut rng)).collect();
        let v = ccs.compute_v_j(&z_1, &r_x);

        let pedersen_params = pedersen::new_params(&mut rng, ccs.n - ccs.l - 1);
        let (running_instance, _) = ccs.to_lcccs(&mut rng, &pedersen_params, &z_1, &r_x, &v);
        let (new_instance, _) = ccs.to_cccs(&mut rng, &pedersen_params, &z_2);

        let multifolding_protocol = Multifolding {
            running_instance,
            new_instance,
        };

        let (sumcheck_proof, sigmas, thetas) = multifolding_protocol.prove(&ccs, &z_1, &z_2);
        multifolding_protocol.verify(&ccs, sumcheck_proof, &sigmas, &thetas);
    }
}
