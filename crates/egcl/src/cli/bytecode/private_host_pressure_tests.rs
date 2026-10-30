// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::{MAX_PRIVATE_HOST_GROWTH, MIN_PRIVATE_HOST_GROWTH, PrivateHostPressure};

const MIB: usize = 1024 * 1024;

#[test]
fn host_pressure_initializes_and_uses_floor_half_and_cap() {
    let policy = PrivateHostPressure::new();
    assert!(!policy.growth_requests_major(0));
    assert!(!policy.growth_requests_major(MIN_PRIVATE_HOST_GROWTH - 1));
    assert!(policy.growth_requests_major(MIN_PRIVATE_HOST_GROWTH));

    policy.reset_after_major(100 * MIB);
    assert!(!policy.growth_requests_major(150 * MIB - 1));
    assert!(policy.growth_requests_major(150 * MIB));

    policy.reset_after_major(1024 * MIB);
    assert!(!policy.growth_requests_major(1024 * MIB + MAX_PRIVATE_HOST_GROWTH - 1));
    assert!(policy.growth_requests_major(1024 * MIB + MAX_PRIVATE_HOST_GROWTH));
}

#[test]
fn host_pressure_lowers_transient_sweep_and_jit_high_watermarks() {
    let policy = PrivateHostPressure::new();
    assert!(!policy.growth_requests_major(512 * MIB));
    // Collector scratch and temporary compilation storage have disappeared.
    assert!(!policy.growth_requests_major(40 * MIB));
    assert!(!policy.growth_requests_major(60 * MIB - 1));
    assert!(policy.growth_requests_major(60 * MIB));
    policy.reset_after_major(256 * MIB);
    assert!(!policy.growth_requests_major(8 * MIB));
    assert!(policy.growth_requests_major(24 * MIB));
}

#[test]
fn host_pressure_uses_count_only_when_tracking_is_unavailable() {
    let policy = PrivateHostPressure::new();
    assert!(!policy.requests_major(false, usize::MAX, 16_383, 16_384));
    assert!(policy.requests_major(false, 0, 16_384, 16_384));
    assert!(!policy.requests_major(true, 0, usize::MAX, 16_384));
    assert!(!policy.requests_major(true, MIN_PRIVATE_HOST_GROWTH - 1, usize::MAX, 16_384));
    assert!(policy.requests_major(true, MIN_PRIVATE_HOST_GROWTH, 1, 16_384));
}
