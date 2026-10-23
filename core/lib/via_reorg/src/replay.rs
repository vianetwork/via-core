//! Immutable-field comparison is ordered so every owner reports the same first conflict.

/// Idea: TigerBeetle's fixed-order replay checks, without its transfer-specific exceptions.
/// <https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine.zig#L3971-L4033>
pub fn first_difference<'a, T: PartialEq + 'a>(
    fields: impl IntoIterator<Item = (&'a str, Option<&'a T>, Option<&'a T>)>,
) -> Option<&'a str> {
    fields.into_iter().find_map(|(name, previous, incoming)| {
        (previous != incoming || previous.is_none()).then_some(name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_compares_every_field_in_declared_order() {
        let fields = ["flags", "amount", "reference"];
        let previous = [1, 2, 3];
        assert_eq!(
            first_difference(fields.iter().zip(&previous).map(|(&name, v)| (
                name,
                Some(v),
                Some(v)
            ))),
            None
        );
        for changed in 0..fields.len() {
            let mut incoming = previous;
            incoming[changed] += 1;
            assert_eq!(
                first_difference((0..fields.len()).map(|i| (
                    fields[i],
                    Some(&previous[i]),
                    Some(&incoming[i])
                ))),
                Some(fields[changed])
            );
        }
        let incoming = [9, 9, 9];
        assert_eq!(
            first_difference((0..fields.len()).map(|i| (
                fields[i],
                Some(&previous[i]),
                Some(&incoming[i])
            ))),
            Some("flags")
        );
        assert_eq!(first_difference::<u8>([("amount", None, None)]), Some("amount"));
        assert_eq!(first_difference([("amount", Some(&1), None)]), Some("amount"));
        assert_eq!(first_difference([("amount", None, Some(&1))]), Some("amount"));
    }
}
