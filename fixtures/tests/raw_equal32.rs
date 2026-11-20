use simplex::simplicityhl::simplicity::BitMachine;
use simplex::simplicityhl::{Arguments, WitnessValues};
use simplex_fixtures::artifacts::raw_equal32::RawEqual32Program;
use simplex_fixtures::artifacts::raw_equal32::derived_raw_equal32::RawEqual32Witness;

#[simplex::test]
fn raw_equal32_executes_hl_witnesses(_: simplex::TestContext) -> anyhow::Result<()> {
    let program = RawEqual32Program::new(Arguments::default());
    let compiled = program.as_ref().load()?;

    for (a, b, expected) in [
        (0, 0, true),
        (42, 42, true),
        (42, 43, false),
        (u32::MAX, u32::MAX, true),
    ] {
        let witnesses = RawEqual32Witness { a, b };
        let values: WitnessValues = (&witnesses).into();
        let satisfied = compiled.satisfy(values).map_err(anyhow::Error::msg)?;
        let mut machine = BitMachine::for_program(satisfied.redeem())?;
        let result = machine.exec(satisfied.redeem(), &simplex::simplicityhl::dummy_env::dummy());
        assert_eq!(result.is_ok(), expected, "equality of {a} and {b}");
    }

    Ok(())
}
