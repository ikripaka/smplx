use std::collections::HashMap;

use simplex::simplicityhl::num::U256;
use simplex::simplicityhl::simplicity::BitMachine;
use simplex::simplicityhl::types::TypeConstructible;
use simplex::simplicityhl::value::ValueConstructible;
use simplex::simplicityhl::{
    Arguments, ResolvedType, TemplateProgramWitness, Value, WitnessNameToValueMap, WitnessValues,
};
use simplex_fixtures::artifacts::raw_results::typed_results::TypedResultsProgram;

/// The witnesses of `typed_results.simf` for input `x`: `x + 1` and the choice `choose.simpl` makes.
fn witnesses(x: [u8; 32], expected_increment: [u8; 32]) -> WitnessValues {
    let value = Value::u256(U256::from_byte_array(x));
    // The most significant bit picks the full value (0) or its low 32 bits (1).
    let expected_choice = if x[0] & 0x80 == 0 {
        Value::left(value.clone(), ResolvedType::u32())
    } else {
        let low = u32::from_be_bytes(x[28..].try_into().unwrap());
        Value::right(ResolvedType::u256(), Value::u32(low))
    };
    WitnessValues::from_map(HashMap::from([
        (TemplateProgramWitness::witness_from_str("X"), value),
        (
            TemplateProgramWitness::witness_from_str("EXPECTED_INCREMENT"),
            Value::u256(U256::from_byte_array(expected_increment)),
        ),
        (
            TemplateProgramWitness::witness_from_str("EXPECTED_CHOICE"),
            expected_choice,
        ),
    ]))
}

/// `x + 1` modulo 2^256, big-endian.
fn increment(mut x: [u8; 32]) -> [u8; 32] {
    for byte in x.iter_mut().rev() {
        let (sum, carry) = byte.overflowing_add(1);
        *byte = sum;
        if !carry {
            break;
        }
    }
    x
}

// The raw files sit in different directories: one below the contract, one beside it.
#[simplex::test]
fn raw_typed_results_from_different_paths(_: simplex::TestContext) -> anyhow::Result<()> {
    let program = TypedResultsProgram::new(Arguments::default());
    let raw_paths: Vec<_> = TypedResultsProgram::RAW_SOURCES.iter().map(|(path, _)| *path).collect();
    assert_eq!(raw_paths, ["arith/identity.simpl", "../raw_lib/choose.simpl"]);
    let compiled = program.as_ref().load()?;

    let mut mid = [0u8; 32];
    mid[31] = 0xff;
    mid[0] = 0x7f;
    for x in [[0u8; 32], mid, [0xff; 32]] {
        for (expected_increment, valid) in [(increment(x), true), (x, false)] {
            let satisfied = compiled
                .satisfy(witnesses(x, expected_increment))
                .map_err(anyhow::Error::msg)?;
            let mut machine = BitMachine::for_program(satisfied.redeem())?;
            let result = machine.exec(satisfied.redeem(), &simplex::simplicityhl::dummy_env::dummy());
            assert_eq!(
                result.is_ok(),
                valid,
                "input {x:02x?}, increment {expected_increment:02x?}"
            );
        }
    }

    Ok(())
}
