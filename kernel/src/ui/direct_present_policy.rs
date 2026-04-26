// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#[inline]
pub const fn should_auto_claim_runtime_display(
    runtime_claimed: bool,
    compositor_declared: bool,
) -> bool {
    !runtime_claimed && !compositor_declared
}

#[cfg(test)]
mod tests {
    use super::should_auto_claim_runtime_display;

    #[test]
    fn auto_claim_when_unclaimed_and_no_compositor() {
        assert!(should_auto_claim_runtime_display(false, false));
    }

    #[test]
    fn no_auto_claim_when_already_claimed() {
        assert!(!should_auto_claim_runtime_display(true, false));
    }

    #[test]
    fn no_auto_claim_when_compositor_declared() {
        assert!(!should_auto_claim_runtime_display(false, true));
    }

    #[test]
    fn no_auto_claim_when_claimed_and_compositor_declared() {
        assert!(!should_auto_claim_runtime_display(true, true));
    }
}
