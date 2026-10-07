//! Independent accounting observations over funding trees.

use skein_lib::List;

use crate::{Numbers, carve, charge, left, settle};

fn add(a: u64, b: u64) -> u64 {
    a.checked_add(b).unwrap()
}

fn multiply(a: u64, b: u64) -> u64 {
    a.checked_mul(b).unwrap()
}

fn remainder(a: u64, b: u64) -> u64 {
    a.checked_rem(b).unwrap()
}

fn fresh(budget: u64) -> Numbers {
    Numbers { budget, spent: 0, spent_below: 0, reserved: 0 }
}

#[test]
fn accounting_refuses_unreserved_spend_and_invalid_numbers() {
    let start = fresh(10);
    assert_eq!(carve(start, &[6, 5]), None, "a batch cannot exceed what is available");
    assert_eq!(carve(start, &[u64::MAX, 1]), None, "the sum of reservations must be representable");
    let reserved = carve(start, &[6, 4]).unwrap();
    assert_eq!(left(reserved), 0);
    assert_eq!(charge(reserved, 3), None, "reserved funding cannot also be charged");
    let charged = charge(carve(start, &[6]).unwrap(), 4).unwrap();
    assert_eq!(charged.spent, 4);
    assert_eq!(charged.reserved, 6);
    assert_eq!(left(charged), 0);
    assert_eq!(settle(start, fresh(1)), None);
    assert_eq!(settle(reserved, reserved), None);
    let huge = Numbers { spent: u64::MAX, reserved: u64::MAX, ..fresh(u64::MAX) };
    assert_eq!(left(huge), 0);
    assert_eq!(charge(huge, 1), None);
    let impossible = Numbers { spent: u64::MAX, spent_below: 1, ..fresh(u64::MAX) };
    assert_eq!(carve(impossible, &[]), None);
    assert_eq!(charge(impossible, 0), None);
    assert_eq!(settle(fresh(0), impossible), None);
    let parent = Numbers { spent: u64::MAX, reserved: 1, ..fresh(u64::MAX) };
    assert_eq!(settle(parent, charge(fresh(1), 1).unwrap()), None);
    assert_eq!(start, fresh(10));
}

#[test]
fn every_expense_is_counted_once_up_generated_funding_trees() {
    for seed in 0..64_u64 {
        let mut root = fresh(10_000);
        let mut expenses = List::with_capacity(17);
        let mut funded = List::with_capacity(4);
        for child in 0..4_u64 {
            let budget = add(100, remainder(add(multiply(seed, 13), multiply(child, 19)), 97));
            root = carve(root, &[budget]).unwrap();
            let mut budgets = List::with_capacity(3);
            for leaf in 0..3_u64 {
                let amount = add(10, remainder(add(add(seed, child), leaf), 13));
                budgets.push(amount).unwrap();
            }
            let mut node = carve(fresh(budget), budgets.as_slice()).unwrap();
            for (leaf, amount) in budgets.iter().enumerate() {
                let actual = remainder(
                    add(add(multiply(seed, 7), multiply(child, 3)), multiply(u64::try_from(leaf).unwrap(), 17)),
                    add(*amount, 1),
                );
                expenses.push(actual).unwrap();
                let charged = charge(fresh(*amount), actual).unwrap();
                node = settle(node, charged).unwrap();
            }
            let actual = remainder(add(multiply(seed, 11), child), add(left(node), 1));
            expenses.push(actual).unwrap();
            let charged = charge(node, actual).unwrap();
            node = charged;
            funded.push(node).unwrap();
        }
        for ended in &funded {
            root = settle(root, *ended).unwrap();
        }
        root = charge(root, 17).unwrap();
        expenses.push(17).unwrap();
        let mut expected = 0_u64;
        for expense in &expenses {
            expected = add(expected, *expense);
        }
        assert_eq!(add(root.spent, root.spent_below), expected, "seed {seed}");
        assert_eq!(root.reserved, 0, "seed {seed}");
        assert_eq!(left(root), 10_000_u64.saturating_sub(expected), "seed {seed}");
    }
}

#[test]
fn a_new_period_does_not_clear_the_reservations_of_its_predecessor() {
    let old = carve(fresh(100), &[80]).unwrap();
    let current = fresh(100);
    assert_eq!(left(old), 20);
    assert_eq!(left(current), 100);
    let ended = charge(fresh(80), 35).unwrap();
    let settled = settle(old, ended).unwrap();
    assert_eq!(settled.spent_below, 35);
    assert_eq!(left(settled), 65);
    assert_eq!(current, fresh(100));
    assert_eq!(charge(fresh(80), 110), None);
    assert_eq!(current.reserved, 0);
}
