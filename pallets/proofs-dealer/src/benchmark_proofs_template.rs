//! Autogenerated proof cases for benchmarking `pallet_proofs_dealer`.
//!
//! THIS FILE WAS AUTO-GENERATED USING THE `multi-files-benchmarking.test.ts` TEST SUITE FROM STORAGE HUB.
//! DATE: {{date}}.
//! 
//! To regenerate this file, follow these steps:
//! 1. Clone the `storage-hub` repo if you haven't already.
//! 2. Make sure you're able to run BSPNet integration tests locally. You can see the setps in the [testing README](https://github.com/Moonsong-Labs/storage-hub/blob/main/test/README.md).
//! 3. In the [multi-files-benchmarking.test.ts](https://github.com/Moonsong-Labs/storage-hub/blob/main/test/suites/integration/bsp/multi-files-benchmarking.test.ts) test suite,
//!    replace the `skip: true` with `only: true`.
//! 4. Run the test suite with `pnpm test:bspnet:only`.
//! 
//! Only the `multi-files-benchmarking.test.ts` test should run, and it should automatically regenerate this file.

#![rustfmt::skip]

use sp_std::vec;

#[rustfmt::skip]
fn fetch_proof(number_of_challenges: u32) -> Vec<Vec<u8>> {
    match number_of_challenges {
        {{proofs}}
        _ => panic!(
            "Number of challenges ({}) is not supported",
            number_of_challenges
        ),
    }
}

#[rustfmt::skip]
fn fetch_challenges(number_of_challenges: u32) -> Vec<Vec<u8>> {
    match number_of_challenges {
        {{challenges}}
        _ => panic!(
            "Number of challenges ({}) is not supported",
            number_of_challenges
        ),
    }
}
