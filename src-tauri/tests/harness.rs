mod support;

use support::{Failpoints, TestLayout};

#[test]
fn fixture_layout_is_isolated_and_complete() {
    let fixture = TestLayout::new();
    let fixture_root = fixture.root().to_path_buf();

    assert!(fixture.home.is_dir());
    assert!(fixture.vault.is_dir());
    assert!(fixture.project.is_dir());
    assert_ne!(fixture.vault, fixture.project);

    drop(fixture);
    assert!(!fixture_root.exists());
}

#[test]
fn failpoints_are_selected_by_name_and_step() {
    let mut failpoints = Failpoints::default();
    failpoints.enable("publish-manifest", 2);

    assert!(failpoints.is_enabled("publish-manifest", 2));
    assert!(!failpoints.is_enabled("publish-manifest", 1));
    assert!(!failpoints.is_enabled("commit-target", 2));
}
