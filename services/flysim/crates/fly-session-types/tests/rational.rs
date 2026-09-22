//! Checked rational arithmetic and the step-v1 section 5 tick accumulator.

use fly_session_types::scalar::{DomainType, RationalNs};
use fly_session_types::fixtures;
use serde_json::Value;

fn rational(value: &Value) -> RationalNs {
    RationalNs::from_json(value).expect("a valid rational")
}

#[test]
fn the_accumulator_produces_the_fixture_tick_counts_and_remainders() {
    let file = fixtures::load("rational.json").expect("rational.json");
    for case in file
        .get("accumulator")
        .and_then(Value::as_array)
        .expect("accumulator")
    {
        let name = fixtures::field(case, "name").expect("name");
        let step = rational(case.get("stepDuration").expect("stepDuration"));
        let tick = rational(case.get("tickDuration").expect("tickDuration"));
        let mut accumulator = RationalNs::ZERO;
        let mut total = 0u64;
        for (index, expected) in case
            .get("steps")
            .and_then(Value::as_array)
            .expect("steps")
            .iter()
            .enumerate()
        {
            accumulator = accumulator.checked_add(&step).expect("checked add");
            let (ticks, remainder) = accumulator.divide_floor(&tick).expect("checked divide");
            accumulator = remainder;
            total += ticks;
            assert_eq!(
                ticks.to_string(),
                fixtures::field(expected, "ticks").expect("ticks"),
                "{name}: tick count at step {index}"
            );
            assert_eq!(
                remainder,
                rational(expected.get("remainder").expect("remainder")),
                "{name}: remainder at step {index}"
            );
            assert!(
                remainder < tick,
                "{name}: the remainder is always less than one model tick"
            );
        }
        assert_eq!(
            total.to_string(),
            fixtures::field(case, "totalTicks").expect("totalTicks"),
            "{name}: total ticks"
        );
    }
}

#[test]
fn checked_arithmetic_reduces_or_refuses() {
    let file = fixtures::load("rational.json").expect("rational.json");
    for case in file.get("add").and_then(Value::as_array).expect("add") {
        let outcome = rational(case.get("a").expect("a")).checked_add(&rational(case.get("b").expect("b")));
        match case.get("sum") {
            Some(sum) => assert_eq!(outcome.expect("a sum"), rational(sum)),
            None => assert!(outcome.is_err(), "the sum must overflow: {case}"),
        }
    }
    for case in file
        .get("subtract")
        .and_then(Value::as_array)
        .expect("subtract")
    {
        let outcome =
            rational(case.get("a").expect("a")).checked_sub(&rational(case.get("b").expect("b")));
        match case.get("difference") {
            Some(difference) => assert_eq!(outcome.expect("a difference"), rational(difference)),
            None => assert!(outcome.is_err(), "the subtraction must fail: {case}"),
        }
    }
    for case in file
        .get("multiply")
        .and_then(Value::as_array)
        .expect("multiply")
    {
        let k: u64 = fixtures::field(case, "k")
            .expect("k")
            .parse()
            .expect("a u64");
        let outcome = rational(case.get("a").expect("a")).checked_mul_u64(k);
        match case.get("product") {
            Some(product) => assert_eq!(outcome.expect("a product"), rational(product)),
            None => assert!(outcome.is_err(), "the product must overflow: {case}"),
        }
    }
    for case in file
        .get("compare")
        .and_then(Value::as_array)
        .expect("compare")
    {
        let left = rational(case.get("a").expect("a"));
        let right = rational(case.get("b").expect("b"));
        let ordering = match fixtures::field(case, "ordering").expect("ordering") {
            "less" => std::cmp::Ordering::Less,
            "equal" => std::cmp::Ordering::Equal,
            "greater" => std::cmp::Ordering::Greater,
            other => panic!("unknown ordering {other:?}"),
        };
        assert_eq!(left.cmp(&right), ordering, "{case}");
    }
}

#[test]
fn zero_has_exactly_one_encoding_and_durations_must_be_positive() {
    assert_eq!(RationalNs::ZERO, RationalNs::new(0, 1).expect("0/1"));
    assert!(RationalNs::new(0, 2).is_err(), "zero is encoded 0/1");
    assert!(RationalNs::new(1, 0).is_err(), "denominators are positive");
    assert!(RationalNs::new(2, 4).is_err(), "fractions are reduced");
    assert!(RationalNs::ZERO.require_positive("worldTime").is_err());
    assert!(
        RationalNs::new(1, 3)
            .expect("1/3")
            .require_positive("tickDuration")
            .is_ok()
    );
    assert!(
        RationalNs::ZERO.divide_floor(&RationalNs::ZERO).is_err(),
        "dividing by a zero tick is refused, not infinite"
    );
}

#[test]
fn reduction_refuses_a_result_that_does_not_fit_u64() {
    let big = RationalNs::new(u64::MAX, 1).expect("a whole number");
    assert!(big.checked_mul_u64(2).is_err());
    assert!(big.checked_add(&big).is_err());
    assert_eq!(
        RationalNs::reduced(u128::from(u64::MAX) * 2, 2).expect("reduces back into range"),
        big
    );
}
